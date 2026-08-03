/**
 * workspaceRestore pure coordinator 测试。
 */
import { describe, expect, it } from 'vitest';
import {
  applyWorkspaceRestorePlan,
  formatRestoreNotice,
  type WorkspaceRestoreBridge,
  type WorkspaceRestorePlan,
  type WorkspaceSelectionSnapshot,
} from './workspaceRestore';

function previous(
  overrides: Partial<WorkspaceSelectionSnapshot> = {},
): WorkspaceSelectionSnapshot {
  return {
    projectId: null,
    worktreeId: null,
    sessionId: null,
    workspaceView: 'terminal',
    inspectorTab: 'files',
    browserTargetUrl: null,
    dirtyEditor: false,
    ...overrides,
  };
}

function completePlan(): WorkspaceRestorePlan {
  return {
    restoreId: 'r1',
    layoutId: 'L1',
    layoutRevision: 1,
    status: 'complete',
    resolvedProjectId: 'p1',
    resolvedWorktreeId: 'w1',
    resolvedSessionId: 's1',
    workspaceView: 'files',
    inspectorTab: 'git',
    browserTargetUrl: 'http://127.0.0.1:5173/',
    actions: [
      { target: 'project', resourceId: 'p1', outcome: 'select' },
      { target: 'worktree', resourceId: 'w1', outcome: 'select' },
      { target: 'session', resourceId: 's1', outcome: 'safeAttach' },
      { target: 'workspaceView', resourceId: 'files', outcome: 'select' },
      { target: 'inspectorTab', resourceId: 'git', outcome: 'select' },
      {
        target: 'browserTarget',
        resourceId: 'http://127.0.0.1:5173/',
        outcome: 'select',
      },
    ],
  };
}

function partialPlan(): WorkspaceRestorePlan {
  const plan = completePlan();
  plan.status = 'partial';
  plan.actions = [
    { target: 'project', resourceId: 'p1', outcome: 'select' },
    { target: 'worktree', resourceId: 'w1', outcome: 'select' },
    { target: 'session', resourceId: 's1', outcome: 'select' },
    {
      target: 'session',
      resourceId: 's-missing',
      outcome: 'skip',
      reason: 'tmuxTargetMissing',
    },
    {
      target: 'browserTarget',
      resourceId: null,
      outcome: 'skip',
      reason: 'browserTargetInvalid',
    },
  ];
  return plan;
}

function bridgeSpy(): WorkspaceRestoreBridge & {
  calls: string[];
  snapshotRestored: boolean;
} {
  const calls: string[] = [];
  return {
    calls,
    snapshotRestored: false,
    selectProject: async (id) => {
      calls.push(`project:${id}`);
    },
    selectWorktree: async (id) => {
      calls.push(`worktree:${id}`);
    },
    focusSession: async (id) => {
      calls.push(`focus:${id}`);
    },
    safeAttachSession: async (id) => {
      calls.push(`attach:${id}`);
    },
    setWorkspaceView: (view) => {
      calls.push(`view:${view}`);
    },
    setInspectorTab: (tab) => {
      calls.push(`inspector:${tab}`);
    },
    restoreBrowserTarget: async (url) => {
      calls.push(`browser:${url}`);
    },
    applySelectionSnapshot: async () => {
      calls.push('rollback');
    },
  };
}

describe('applyWorkspaceRestorePlan', () => {
  it('waits for preflight before selection and applies in order', async () => {
    const bridge = bridgeSpy();
    let preflightStarted = false;
    let selectionBeforePreflight = false;
    const summary = await applyWorkspaceRestorePlan({
      previous: previous(),
      preflight: async () => {
        preflightStarted = true;
        expect(bridge.calls).toHaveLength(0);
        return completePlan();
      },
      bridge: {
        ...bridge,
        selectProject: async (id) => {
          if (!preflightStarted) selectionBeforePreflight = true;
          await bridge.selectProject(id);
        },
      },
    });
    expect(selectionBeforePreflight).toBe(false);
    expect(bridge.calls[0]).toBe('project:p1');
    expect(bridge.calls).toEqual([
      'project:p1',
      'worktree:w1',
      'attach:s1',
      'view:files',
      'inspector:git',
      'browser:http://127.0.0.1:5173/',
    ]);
    expect(summary?.silent).toBe(true);
    expect(summary?.restoredCount).toBe(6);
  });

  it('summarizes partial restore once and preserves dirty editor', async () => {
    const bridge = bridgeSpy();
    const summary = await applyWorkspaceRestorePlan({
      previous: previous({ dirtyEditor: true }),
      preflight: async () => partialPlan(),
      bridge,
    });
    expect(summary?.silent).toBe(false);
    expect(summary?.skippedCount).toBe(2);
    expect(summary?.dirtyEditorPreserved).toBe(true);
    expect(formatRestoreNotice(summary!)).toBe('已恢复 3 项，2 项已跳过');
  });

  it('rolls back previous selection on apply exception', async () => {
    const bridge = bridgeSpy();
    const rolling = {
      ...bridge,
      selectWorktree: async () => {
        throw new Error('boom');
      },
    };
    const summary = await applyWorkspaceRestorePlan({
      previous: previous({ projectId: 'old' }),
      preflight: async () => completePlan(),
      bridge: rolling,
    });
    expect(rolling.calls).toContain('rollback');
    expect(summary?.reasons).toContain('applyException');
  });

  it('skips mobile auto-apply of desktop layout', async () => {
    const bridge = bridgeSpy();
    const summary = await applyWorkspaceRestorePlan({
      previous: previous(),
      preflight: async () => completePlan(),
      bridge,
      isMobile: true,
    });
    expect(summary).toBeNull();
    expect(bridge.calls).toHaveLength(0);
  });

  /**
   * 2026-08-03 backend fix：plan 只有当 layout.workspace_view == 'browser'
   * 时才会包含 target='browserTarget' 且 outcome='select' 的 action。
   * 若 plan 只发 workspaceView=terminal（典型：用户上次停在 terminal），
   * apply 阶段绝对不应触发 bridge.restoreBrowserTarget，从而避免把视图
   * 强制切回 browser。这是 useWorkspaceSafeRestore.ts 的 bridge 注释
   * 「此处不走 forceTerminalWorkspaceView 强制逻辑」在前端层的护栏。
   */
  it('does not invoke restoreBrowserTarget when plan omits browser Select action', async () => {
    const bridge = bridgeSpy();
    // 模拟后端 preflight_workspace_restore 的新行为：workspaceView=terminal
    // 时不发 browserTarget Select；plan.browserTargetUrl 字段仍保留供 UI
    // placeholder 使用。
    const terminalOnlyPlan: WorkspaceRestorePlan = {
      restoreId: 'r2',
      layoutId: 'L2',
      layoutRevision: 2,
      status: 'complete',
      resolvedProjectId: 'p1',
      resolvedWorktreeId: 'w1',
      resolvedSessionId: null,
      workspaceView: 'terminal',
      inspectorTab: 'files',
      browserTargetUrl: 'http://127.0.0.1:3000',
      actions: [
        { target: 'project', resourceId: 'p1', outcome: 'select' },
        { target: 'worktree', resourceId: 'w1', outcome: 'select' },
        {
          target: 'workspaceView',
          resourceId: 'terminal',
          outcome: 'select',
        },
        {
          target: 'inspectorTab',
          resourceId: 'files',
          outcome: 'select',
        },
      ],
    };
    const summary = await applyWorkspaceRestorePlan({
      previous: previous(),
      preflight: async () => terminalOnlyPlan,
      bridge,
    });
    expect(bridge.calls).toEqual([
      'project:p1',
      'worktree:w1',
      'view:terminal',
      'inspector:files',
    ]);
    // plan 含 browserTargetUrl 但不含 browser Select action，
    // apply 不应调 restoreBrowserTarget（关键护栏）。
    expect(bridge.calls.some((call) => call.startsWith('browser:'))).toBe(false);
    expect(summary?.silent).toBe(true);
    expect(summary?.restoredCount).toBe(4);
  });
});
