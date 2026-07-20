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
import { describe, expect, test, vi } from 'vitest';
import { fireEvent, screen } from '@testing-library/react';
import { createTerminalInputPump } from './terminalInputPump';
import { createTerminalLiveWriter } from './terminalLiveWriter';
import { createWorkbenchTerminalBufferStore } from '@/hooks/workbenchTerminalBuffer';

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

    // terminalLayer 仍存在（常驻语义），仅 data-hidden 切换；browser workspace 挂载（lazy）。
    const terminalLayerAfter = document.querySelector('[class*="terminalLayer"]');
    expect(terminalLayerAfter).toBeTruthy();
    expect(terminalLayerAfter?.getAttribute('data-hidden')).toBe('true');
    await waitFor(() => {
      if (!screen.queryByTestId('workbench-browser-workspace')) {
        throw new Error('browser workspace not mounted');
      }
    });
  });

  test('session tabs use roving tabIndex and activate with Arrow/Home/End', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const sessions = [
      buildSession({ id: 's1', name: 'shell-1' }),
      buildSession({ id: 's2', name: 'shell-2' }),
      buildSession({ id: 's3', name: 'shell-3' }),
    ];
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return [worktree];
        case 'list_workbench_sessions':
          return sessions;
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

    const tab1 = screen.getByRole('tab', { name: /shell-1/ });
    const tab2 = screen.getByRole('tab', { name: /shell-2/ });
    const tab3 = screen.getByRole('tab', { name: /shell-3/ });
    expect(tab1.getAttribute('tabindex')).toBe('0');
    expect(tab2.getAttribute('tabindex')).toBe('-1');
    expect(tab3.getAttribute('tabindex')).toBe('-1');
    // close 是 sibling 按钮，不嵌套在 tab 内
    expect(tab1.querySelector('button')).toBeNull();

    tab1.focus();
    fireEvent.keyDown(tab1, { key: 'ArrowRight' });
    await waitFor(() => {
      if (screen.getByRole('tab', { name: /shell-2/ }).getAttribute('aria-selected') !== 'true') {
        throw new Error('ArrowRight did not activate shell-2');
      }
    });
    expect(invokeCallsFor('focus_workbench_session').some((c) => c.args.sessionId === 's2')).toBe(
      true,
    );

    fireEvent.keyDown(screen.getByRole('tab', { name: /shell-2/ }), { key: 'End' });
    await waitFor(() => {
      if (screen.getByRole('tab', { name: /shell-3/ }).getAttribute('aria-selected') !== 'true') {
        throw new Error('End did not activate shell-3');
      }
    });

    fireEvent.keyDown(screen.getByRole('tab', { name: /shell-3/ }), { key: 'Home' });
    await waitFor(() => {
      if (screen.getByRole('tab', { name: /shell-1/ }).getAttribute('aria-selected') !== 'true') {
        throw new Error('Home did not activate shell-1');
      }
    });

    fireEvent.keyDown(screen.getByRole('tab', { name: /shell-1/ }), { key: 'ArrowLeft' });
    await waitFor(() => {
      if (screen.getByRole('tab', { name: /shell-3/ }).getAttribute('aria-selected') !== 'true') {
        throw new Error('ArrowLeft wrap did not activate shell-3');
      }
    });
  });

  test('closing selected session focuses adjacent tab or new-session button', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    let liveSessions = [
      buildSession({ id: 's1', name: 'shell-1' }),
      buildSession({ id: 's2', name: 'shell-2' }),
    ];
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return [worktree];
        case 'list_workbench_sessions':
          return liveSessions;
        case 'list_workbench_git_commits':
          return [];
        case 'list_workbench_dir':
          return [];
        case 'focus_workbench_session':
          return { ok: true, sessionId: call.args.sessionId };
        case 'close_workbench_session':
          liveSessions = liveSessions.filter((session) => session.id !== call.args.sessionId);
          return { ok: true };
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: project.id }),
      buildDependencyContextValue(),
    );
    await waitForInvoke('focus_workbench_session');

    const tablist = screen.getByRole('tablist', { name: '终端会话' });
    const closeInTabs = () =>
      Array.from(tablist.querySelectorAll('button')).filter(
        (node) => node.getAttribute('aria-label') === '关闭终端',
      );
    expect(closeInTabs().length).toBe(2);
    // 关闭当前选中 s1，焦点应落到相邻 s2
    fireEvent.click(closeInTabs()[0]);
    await waitForInvoke('close_workbench_session');
    await waitFor(() => {
      const next = document.getElementById('workbench-session-tab-s2');
      if (document.activeElement !== next) {
        throw new Error(`expected focus on s2 tab, got ${document.activeElement?.id ?? 'none'}`);
      }
    });

    // 再关最后一个，焦点落到新建终端
    fireEvent.click(closeInTabs()[0]);
    await waitForInvoke('close_workbench_session');
    await waitFor(() => {
      const next = document.getElementById('workbench-session-tab-new');
      if (document.activeElement !== next) {
        throw new Error(`expected focus on new session, got ${document.activeElement?.id ?? 'none'}`);
      }
    });
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


describe('Workbench terminal latency contracts (Task 8)', () => {
  test('1000 ordered inputs coalesce with max concurrency 1 and exact payload', async () => {
    /**
     * Business Logic（为什么需要这个测试）:
     *   快速输入时必须 per-session 串行提交且拼接结果精确相等，失败不得重放。
     *
     * Code Logic（这个测试做什么）:
     *   fake writer 首批 defer；连续 enqueue 1000 个确定字符/控制序列；
     *   settle 后断言拼接精确相等、并发峰值 1。
     */
    let resolveFirst!: () => void;
    const firstPromise = new Promise<void>((ok) => {
      resolveFirst = ok;
    });
    let inFlight = 0;
    let maxInFlight = 0;
    const writes: string[] = [];
    const write = vi.fn(async (_sessionId: string, data: string) => {
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
      writes.push(data);
      try {
        if (writes.length === 1) {
          await firstPromise;
        }
      } finally {
        inFlight -= 1;
      }
    });
    const pump = createTerminalInputPump({ write });
    const parts: string[] = [];
    for (let i = 0; i < 1000; i += 1) {
      const piece =
        i % 17 === 0
          ? '\u007f'
          : i % 23 === 0
            ? '\u001b[D'
            : String.fromCharCode(97 + (i % 26));
      parts.push(piece);
      pump.enqueue('s-latency', piece);
    }
    expect(writes.length).toBe(1);
    expect(maxInFlight).toBe(1);
    resolveFirst();
    await pump.whenIdle('s-latency');
    expect(maxInFlight).toBe(1);
    expect(writes.join('')).toBe(parts.join(''));
  });

  test('live delta reaches terminal.write before manual rAF scheduler flush', () => {
    /**
     * Business Logic（为什么需要这个测试）:
     *   已挂载 xterm 的 live 路径不得等 rAF/React revision 才写入。
     *
     * Code Logic（这个测试做什么）:
     *   注入 deferred frameScheduler；append 后立刻断言 write 调用发生在 flush 前。
     *   这不是 wall-clock 测试。
     */
    let scheduled: Array<() => void> = [];
    const frameScheduler = {
      schedule(cb: () => void) {
        scheduled.push(cb);
        return () => {
          scheduled = scheduled.filter((item) => item !== cb);
        };
      },
    };
    const store = createWorkbenchTerminalBufferStore({ frameScheduler });
    store.reset('s-live', '');
    const writes: string[] = [];
    const terminal = {
      clear() {},
      write(data: string, cb?: () => void) {
        writes.push(data);
        cb?.();
      },
    };
    const writer = createTerminalLiveWriter({
      terminal,
      source: store,
      sessionId: 's-live',
    });
    writes.length = 0;
    scheduled = [];
    store.append('s-live', 'live-fast');
    expect(writes).toEqual(['live-fast']);
    for (const cb of [...scheduled]) cb();
    expect(writes).toEqual(['live-fast']);
    writer.dispose();
  });
});
