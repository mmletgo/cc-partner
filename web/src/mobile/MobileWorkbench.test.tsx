// @vitest-environment jsdom
/**
 * MobileWorkbench 项目重试与连接态合同测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   首次详情失败后同项目必须可重试；切项目取消旧请求；error 按钮必须 force reload。
 *
 * Code Logic（这个测试做什么）:
 *   mock HTTP transport，覆盖失败同项目重试与 ready 早退。
 */

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';

const listProjectsMock = vi.fn();
const listWorktreesMock = vi.fn();
const listSessionsMock = vi.fn();

vi.mock('@/api/workbenchHttp', () => ({
  createHttpOrchestratorClientRequestId: () => 'op-test',
  workbenchHttp: {
    git: {
      commit: vi.fn(),
      push: vi.fn(),
      merge: vi.fn(),
      remove: vi.fn(),
      getMutationOperation: vi.fn(),
    },
    projects: {
      remove: vi.fn(),
    },
    fs: {
      roots: vi.fn(async () => []),
      listDir: vi.fn(async () => []),
      info: vi.fn(),
    },
    remote: {
      roots: vi.fn(async () => []),
      listDir: vi.fn(async () => []),
      info: vi.fn(),
      openProject: vi.fn(),
    },
  },
  httpWorkbenchTransport: {
    projects: {
      list: (...args: unknown[]) => listProjectsMock(...args),
      open: vi.fn(),
    },
    worktrees: {
      list: (...args: unknown[]) => listWorktreesMock(...args),
      create: vi.fn(),
      commit: vi.fn(),
      push: vi.fn(),
      merge: vi.fn(),
      remove: vi.fn(),
    },
    sessions: {
      list: (...args: unknown[]) => listSessionsMock(...args),
      create: vi.fn(),
      focus: vi.fn(async () => ({ ok: true, sessionId: '' })),
      replay: vi.fn(async () => ({
        sessionId: 'p1:s1',
        buffer: '',
        lastSeq: 0,
        truncated: false,
      })),
      hydrateScrollback: vi.fn(async () => ({
        sessionId: 'p1:s1',
        buffer: '',
        lastSeq: 0,
        truncated: false,
      })),
      resize: vi.fn(async () => undefined),
      zoomPane: vi.fn(async () => undefined),
    },
    git: {
      listCommits: vi.fn(async () => []),
    },
    files: {},
    browser: {},
    prompt: {},
  },
}));

vi.mock('@/hooks/attentionContext', () => ({
  useAttention: () => ({
    snapshot: null,
    loading: false,
    refreshing: false,
    stale: false,
    error: null,
    lastSucceededAt: null,
    refresh: vi.fn(async () => undefined),
    markRead: vi.fn(async () => undefined),
    markUnread: vi.fn(async () => undefined),
    markAllRead: vi.fn(async () => undefined),
    markCategoryRead: vi.fn(async () => undefined),
    pendingReadIds: new Set<string>(),
    markError: null,
  }),
}));

vi.mock('@/hooks/workbenchTerminalBuffersContext', () => {
  const store = {
    getBuffer: () => '',
    getRevision: () => 0,
    getSnapshot: () => ({ buffer: '', cursor: { generation: 0 } }),
    append: vi.fn(),
    reset: vi.fn(),
    remove: vi.fn(),
    subscribe: () => () => undefined,
    subscribeLive: () => () => undefined,
    subscribeReset: () => () => undefined,
  };
  return {
    useWorkbenchTerminalBuffers: () => ({
      store,
      resetBuffer: vi.fn(),
      removeBuffer: vi.fn(),
      getHistorySyncFailure: () => null,
      subscribeHistorySyncFailures: () => () => undefined,
      getHistorySyncFailuresRevision: () => 0,
      retryHistorySync: () => undefined,
      refreshScrollback: () => undefined,
      getStartupBaselineFailure: () => null,
      subscribeStartupBaselineFailure: () => () => undefined,
      getStartupBaselineFailureRevision: () => 0,
      retryStartupBaseline: () => undefined,
    }),
    useWorkbenchTerminalBufferStore: () => store,
  };
});

vi.mock('@xterm/xterm', () => ({
  Terminal: class {
    cols = 80;
    rows = 24;
    options = {};
    buffer = { active: { type: 'normal', baseY: 0, viewportY: 0 } };
    modes = { mouseTrackingMode: 'none' };
    loadAddon(): void {}
    open(): void {}
    onData(): { dispose: () => void } {
      return { dispose: () => undefined };
    }
    write(_data: string, cb?: () => void): void {
      cb?.();
    }
    clear(): void {}
    scrollLines(): void {}
    scrollToLine(): void {}
    reset(): void {}
    dispose(): void {}
    blur(): void {}
    focus(): void {}
  },
}));

vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class {
    fit(): void {}
  },
}));

import { MobileWorkbench } from './MobileWorkbench';

/**
 * Business Logic（为什么需要这个函数）:
 *   异步加载测试需要 deferred promise 控制 settle 时序。
 *
 * Code Logic（这个函数做什么）:
 *   返回 promise 与 resolve/reject 控制器。
 */
function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要最小合法项目 DTO。
 *
 * Code Logic（这个函数做什么）:
 *   补齐 WorkbenchProject 必填字段。
 */
function createProject(id = 'p1'): WorkbenchProject {
  return {
    id,
    name: `Project ${id}`,
    kind: 'local',
    deviceId: 'device-1',
    deviceName: 'This Mac',
    path: `/tmp/${id}`,
    lastOpenedAt: '2026-07-14T00:00:00Z',
    createdAt: '2026-07-14T00:00:00Z',
    updatedAt: '2026-07-14T00:00:00Z',
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   详情成功路径需要合法 worktree 列表。
 *
 * Code Logic（这个函数做什么）:
 *   返回主 worktree 样本。
 */
function createWorktree(projectId = 'p1'): WorkbenchWorktree {
  return {
    id: `${projectId}:main`,
    projectId,
    name: 'main',
    branch: 'main',
    baseBranch: null,
    path: `/tmp/${projectId}`,
    isMain: true,
    canCollectMerge: false,
    homeBranch: null,
    collectibleBranches: [],
    status: {
      branch: 'main',
      changed: 0,
      ahead: 0,
      behind: 0,
      conflicts: 0,
      clean: true,
      canPush: false,
    },
    createdAt: '2026-07-14T00:00:00Z',
    updatedAt: '2026-07-14T00:00:00Z',
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   详情成功路径需要合法 session 列表。
 *
 * Code Logic（这个函数做什么）:
 *   返回 running session 样本。
 */
function createSession(projectId = 'p1'): WorkbenchSession {
  return {
    id: `${projectId}:s1`,
    projectId,
    worktreeId: `${projectId}:main`,
    name: 'window-1',
    command: 'zsh',
    cwd: `/tmp/${projectId}`,
    status: 'running',
    cols: 80,
    rows: 24,
    startedAt: '2026-07-14T00:00:00Z',
    exitedAt: null,
    exitCode: null,
    supportsPanes: true,
    paneCount: 1,
  };
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

beforeEach(() => {
  window.history.replaceState(null, '', '/mobile');
  listProjectsMock.mockReset();
  listWorktreesMock.mockReset();
  listSessionsMock.mockReset();
  listProjectsMock.mockResolvedValue([createProject('p1'), createProject('p2')]);
  listWorktreesMock.mockResolvedValue([createWorktree('p1')]);
  listSessionsMock.mockResolvedValue([createSession('p1')]);
  if (typeof globalThis.ResizeObserver === 'undefined') {
    globalThis.ResizeObserver = class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    } as typeof ResizeObserver;
  }
});

afterEach(() => {
  cleanup();
  window.history.replaceState(null, '', '/mobile');
});

/**
 * Business Logic（为什么需要这个函数）:
 *   组件必须挂 i18n 才能断言中文按钮文案。
 *
 * Code Logic（这个函数做什么）:
 *   I18nextProvider 包裹 MobileWorkbench。
 */
function renderWorkbench(): ReturnType<typeof render> {
  return render(
    <I18nextProvider i18n={i18n}>
      <MobileWorkbench />
    </I18nextProvider>,
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   打开项目是重试场景的前置步骤。
 *
 * Code Logic（这个函数做什么）:
 *   等待项目列表后点击目标项目名。
 */
async function openProject(name: string): Promise<void> {
  await screen.findByText(name);
  fireEvent.click(screen.getByText(name));
}

describe('MobileWorkbench project retry and connection', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   首次详情失败后同项目点击“重新加载项目”必须真正重试。
   *
   * Code Logic（这个测试做什么）:
   *   第一次 list worktrees 失败，点击 reload 后断言第二次调用。
   */
  test('clicking the failed active project retries details', async () => {
    listWorktreesMock
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce([createWorktree('p1')]);
    listSessionsMock
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce([createSession('p1')]);

    renderWorkbench();
    await openProject('Project p1');

    await screen.findByRole('button', { name: '重新加载项目' });
    expect(listWorktreesMock).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole('button', { name: '重新加载项目' }));

    await waitFor(() => {
      expect(listWorktreesMock).toHaveBeenCalledTimes(2);
    });
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   切项目时旧详情响应不得覆盖新项目；旧请求应被 requestId 丢弃。
   *
   * Code Logic（这个测试做什么）:
   *   p1 详情 deferred，先点 p2 完成，再 resolve p1，断言状态栏不是 p1。
   */
  test('switching project discards stale detail responses', async () => {
    const p1Worktrees = deferred<WorkbenchWorktree[]>();
    const p1Sessions = deferred<WorkbenchSession[]>();
    listWorktreesMock.mockImplementation((projectId: string) => {
      if (projectId === 'p1') return p1Worktrees.promise;
      return Promise.resolve([createWorktree('p2')]);
    });
    listSessionsMock.mockImplementation((projectId: string) => {
      if (projectId === 'p1') return p1Sessions.promise;
      return Promise.resolve([createSession('p2')]);
    });

    renderWorkbench();
    await openProject('Project p1');
    await openProject('Project p2');

    await waitFor(() => {
      expect(screen.getAllByText('Project p2').length).toBeGreaterThan(0);
    });

    await act(async () => {
      p1Worktrees.resolve([createWorktree('p1')]);
      p1Sessions.resolve([createSession('p1')]);
      await Promise.resolve();
    });

    expect(screen.queryByText('Project p1', { selector: '.topMeta' })).toBeNull();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   ready 同项目可早退，不再重复拉详情。
   *
   * Code Logic（这个测试做什么）:
   *   成功打开后再次点击同项目，listWorktrees 调用次数不增加。
   */
  test('ready same project does not reload details', async () => {
    renderWorkbench();
    await openProject('Project p1');
    await waitFor(() => {
      expect(listWorktreesMock).toHaveBeenCalledTimes(1);
    });
    // 成功后可能切到 terminal；回到 projects 再点
    // 若已不在 projects，导航不一定存在；直接再点项目名可能在 status 区
    const projectButtons = screen.getAllByText('Project p1');
    fireEvent.click(projectButtons[0]!);
    await waitFor(() => {
      expect(listWorktreesMock).toHaveBeenCalledTimes(1);
    });
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   打开项目默认进入终端；worktree 条必须出现在窗口 tab 上方，不能只挂在 shell chrome。
   *
   * Code Logic（这个测试做什么）:
   *   打开项目后等待 lazy 终端面板，断言 mobile-worktree-tabs 与主 worktree chip，
   *   且终端页不渲染 shell chrome 容器。
   */
  test('opening a project shows the worktree strip above terminal window tabs', async () => {
    renderWorkbench();
    await openProject('Project p1');
    await waitFor(() => {
      expect(screen.getByTestId('mobile-worktree-tabs')).toBeTruthy();
    });
    expect(screen.queryByTestId('mobile-worktree-chrome')).toBeNull();
    expect(screen.getByTestId('mobile-worktree-tabs').textContent).toMatch(/main/);
  });
});

describe('MobileWorkbench location restore', () => {
  test('refreshing a terminal workbench URL reopens that project instead of the list', async () => {
    window.history.replaceState(
      null,
      '',
      '/mobile?projectId=p1&panel=terminal&worktreeId=p1%3Amain&sessionId=p1%3As1',
    );
    renderWorkbench();

    await waitFor(() => {
      expect(listWorktreesMock).toHaveBeenCalledWith('p1');
    });
    await waitFor(() => {
      expect(screen.getByTestId('mobile-worktree-tabs')).toBeTruthy();
    });
    expect(screen.queryByRole('heading', { name: '项目' })).toBeNull();
    expect(window.location.search).toContain('projectId=p1');
  });

  test('opening a project writes the workbench into the URL', async () => {
    renderWorkbench();
    await openProject('Project p1');
    await waitFor(() => {
      expect(screen.getByTestId('mobile-worktree-tabs')).toBeTruthy();
      expect(window.location.search).toContain('projectId=p1');
      expect(window.location.search).toContain('panel=terminal');
      expect(window.location.search).toContain('worktreeId=p1%3Amain');
      expect(window.location.search).toContain('sessionId=p1%3As1');
    });
  });

  test('returning to the project list clears workbench query params', async () => {
    renderWorkbench();
    await openProject('Project p1');
    await waitFor(() => {
      expect(screen.getByTestId('mobile-worktree-tabs')).toBeTruthy();
    });

    fireEvent.click(screen.getAllByTestId('mobile-nav-back-to-projects')[0]!);

    await waitFor(() => {
      expect(window.location.search).toBe('');
    });
    expect(screen.getByRole('heading', { name: '项目' })).toBeTruthy();
  });

  test('unknown projectId in the URL stays on the project list', async () => {
    window.history.replaceState(null, '', '/mobile?projectId=missing&panel=terminal');
    renderWorkbench();

    await screen.findByRole('heading', { name: '项目' });
    expect(listWorktreesMock).not.toHaveBeenCalled();
    await waitFor(() => {
      expect(window.location.search).toBe('');
    });
  });
});

