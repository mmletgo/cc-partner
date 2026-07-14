// @vitest-environment jsdom
/**
 * Workbench 自动化域 characterization 测试。
 *
 * Business Logic: 锁住“项目自动化”开关、OrchestratorPanel 任务 deep link 回跳、以及 staged deep link
 * （projectId/worktreeId/sessionId 三段式定位）等当前可观察行为。
 *
 * Code Logic: 通过点击“项目自动化”按钮切换 automationConsoleOpen；通过桩 OrchestratorPanel 的 open
 * 按钮触发 handleOpenAutomationTaskWorkbench → navigate；通过 initialSearch 注入 deep link。
 */
import { describe, expect, test } from 'vitest';
import { fireEvent, screen } from '@testing-library/react';

import {
  buildDependencyContextValue,
  buildLocalProject,
  buildProjectsContextValue,
  buildSession,
  buildWorktree,
  flushMacrotasks,
  invokeCallsFor,
  renderWorkbench,
  setInvokeHandler,
  waitFor,
  waitForInvoke,
} from './testing/workbenchTestHarness';

async function settle(): Promise<void> {
  await flushMacrotasks();
}

describe('Workbench automation domain (characterization)', () => {
  test('toggling project automation opens/closes the embedded OrchestratorPanel', async () => {
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
    await settle();

    // 初始 automation 控制台未打开：桩未挂载。
    expect(screen.queryByTestId('orchestrator-panel')).toBeNull();

    // 点击“项目自动化”按钮 → 嵌入面板出现，按钮 aria-pressed=true。
    fireEvent.click(screen.getByRole('button', { name: '项目自动化' }));
    await waitFor(() => {
      if (!screen.queryByTestId('orchestrator-panel')) {
        throw new Error('orchestrator panel not mounted');
      }
    });
    const panel = screen.getByTestId('orchestrator-panel');
    expect(panel.getAttribute('data-embedded')).toBe('true');

    // 再次点击关闭：桩卸载。
    fireEvent.click(screen.getByRole('button', { name: '项目自动化' }));
    await waitFor(() => {
      if (screen.queryByTestId('orchestrator-panel')) {
        throw new Error('orchestrator panel still mounted after toggle off');
      }
    });
  });

  test('zero projects hide automation chrome and show empty launch CTA', async () => {
    // N4 启动表面：零项目不再渲染完整 Workbench chrome / 禁用的自动化按钮，
    // 只展示聚焦空态 CTA（添加本机/连接远端/检查 tmux）。
    setInvokeHandler(() => ({ ok: true }));
    renderWorkbench(
      buildProjectsContextValue({ projects: [], activeProjectId: null }),
      buildDependencyContextValue(),
    );
    await settle();

    expect(screen.getByTestId('workbench-launch-empty')).toBeTruthy();
    expect(screen.queryByRole('button', { name: '项目自动化' })).toBeNull();
  });

  test('OrchestratorPanel open-workbench callback navigates via deep link and closes console', async () => {
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
    await settle();

    // 打开自动化控制台，点击桩的 open-workbench 按钮。
    fireEvent.click(screen.getByRole('button', { name: '项目自动化' }));
    await waitFor(() => screen.getByTestId('orchestrator-panel'));
    fireEvent.click(screen.getByTestId('orchestrator-open-workbench'));
    await settle();

    // 回跳后自动化控制台应关闭（桩卸载）。
    await waitFor(() => {
      if (screen.queryByTestId('orchestrator-panel')) {
        throw new Error('orchestrator panel should close after deep link navigation');
      }
    });
  });

  test('staged project→worktree→session deep link focuses the referenced session', async () => {
    const project = buildLocalProject({ id: 'p1' });
    const mainWt = buildWorktree({ id: 'wt-main', projectId: 'p1', name: 'main', branch: 'main' });
    const targetSession = buildSession({
      id: 's-target',
      projectId: 'p1',
      worktreeId: 'wt-main',
      name: 'target shell',
    });

    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return [mainWt];
        case 'list_workbench_sessions':
          return [targetSession];
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

    // 通过 initialSearch 注入 staged deep link。
    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: 'p1' }),
      buildDependencyContextValue(),
      { initialSearch: '?projectId=p1&worktreeId=wt-main&sessionId=s-target' },
    );

    // deep link 应用后应触发 focus_workbench_session(target)。
    await waitForInvoke('focus_workbench_session');
    const focusCalls = invokeCallsFor('focus_workbench_session');
    expect(focusCalls.some((c) => c.args.sessionId === 's-target')).toBe(true);
  });
});

// 局部 import：本文件用到的 invokeCallsFor 来自 harness，单独 import 避免与上方重复。
