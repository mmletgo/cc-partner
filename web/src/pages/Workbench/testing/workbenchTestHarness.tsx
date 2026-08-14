/**
 * Workbench characterization harness — deterministic deferred/API/event fakes.
 *
 * Business Logic（为什么需要这个模块）:
 *   Plan 2 的 controller 抽取是一次大型行为保持型重构，必须在改动前锁住 Workbench 当前可观察行为。
 *   真实页面通过 Tauri invoke、xterm、CodeMirror 和 OrchestratorPanel 等重资源驱动，单元测试必须替换为可控制的桩。
 *
 * Code Logic（这个模块做什么）:
 *   - 通过顶层 `vi.mock('@/api/client')` 接管所有 invoke 命令，路由到共享 `workbenchTestState.invokeHandler`；
 *     调用日志（`invokeCalls`）暴露给测试断言，并允许测试动态替换 handler。
 *   - 通过顶层 `vi.mock('@tauri-apps/api/event')` 接管 listen，测试可主动 `emitWorkbenchEvent` 触发事件。
 *   - 顶层桩掉 xterm / FitAddon / OrchestratorPanel / WorkbenchFileWorkspace /
 *     WorkbenchBrowserWorkspace / WorkbenchSessionSearch / WorkbenchDependencyCard，
 *     让 DOM 测试只关注 Workbench 页面本身的可观察行为。
 *   - 提供 `renderWorkbench`：用 MemoryRouter + 项目/依赖/终端 buffer context 把页面挂起来。
 *
 * 重要：本模块顶层调用 `vi.mock`；只要任一 characterization 测试 import 本模块，对应 mock 就会注册到
 * vitest 全局 mock registry，从而接管 Workbench 及其依赖的真实实现。
 */

import { afterEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import type { ReactElement } from 'react';
import { MemoryRouter } from 'react-router-dom';

import { Workbench } from '../Workbench';
import { WorkbenchProjectsContext } from '@/hooks/workbenchProjectsContext';
import type { WorkbenchProjectsContextValue } from '@/hooks/workbenchProjectsContext';
import { WorkbenchDependencyContext } from '@/hooks/workbenchDependencyContext';
import type { WorkbenchDependencyContextValue } from '@/hooks/workbenchDependencyContext';
import { WorkbenchTerminalBuffersProvider } from '@/hooks/useWorkbenchTerminalBuffers';
import { AttentionProvider } from '@/hooks/useAttention';
import type { AttentionSnapshot } from '@/lib/types';
import type {
  PromptOptimizerFillLanguage,
  WorkbenchFileNode,
  WorkbenchGitCommit,
  WorkbenchPathInfo,
  WorkbenchProject,
  WorkbenchSession,
  WorkbenchWorktree,
} from '@/lib/types';
import type { WorkbenchProjectSessionStats } from '@/lib/workbenchProjectStats';
import i18n from '@/i18n';

/* ---------------------------------------------------------------------------
 * jsdom 全局 polyfill：ResizeObserver / IntersectionObserver / matchMedia
 *
 * Business Logic: Workbench TerminalPane 使用 ResizeObserver 监听终端 viewport；jsdom 默认不提供该 API，
 * 必须在测试 import 页面前安装一个空实现，否则 effect 阶段会 ReferenceError。
 * ------------------------------------------------------------------------- */

class EmptyObserver {
  observe(): void {
    /* no-op */
  }
  unobserve(): void {
    /* no-op */
  }
  disconnect(): void {
    /* no-op */
  }
  takeRecords(): unknown[] {
    return [];
  }
}

if (typeof window !== 'undefined') {
  if (!window.ResizeObserver) {
    (window as unknown as { ResizeObserver: unknown }).ResizeObserver = EmptyObserver;
  }
  if (!(window as unknown as { IntersectionObserver?: unknown }).IntersectionObserver) {
    (window as unknown as { IntersectionObserver: unknown }).IntersectionObserver = EmptyObserver;
  }
  if (!window.matchMedia) {
    window.matchMedia = (query: string): MediaQueryList =>
      ({
        matches: false,
        media: query,
        onchange: null,
        addListener: () => undefined,
        removeListener: () => undefined,
        addEventListener: () => undefined,
        removeEventListener: () => undefined,
        dispatchEvent: () => false,
      }) as MediaQueryList;
  }
  // Tauri 用 crypto.randomUUID 生成 listener id；node test 环境自带，但兜底。
  if (!(window as unknown as { crypto?: Crypto }).crypto) {
    (window as unknown as { crypto: Crypto }).crypto = globalThis.crypto;
  }
}

/* ---------------------------------------------------------------------------
 * Shared mutable test state（vi.hoisted 保证在 mock factory 执行前已初始化）
 * ------------------------------------------------------------------------- */

/**
 * 共享的可变测试状态。
 *
 * Business Logic: vi.mock 的 factory 在被 mock 的模块首次 import 时执行，必须引用一个已经存在的对象；
 * 使用 vi.hoisted 让 state 在所有 import 之前完成初始化。
 */
export interface WorkbenchTestState {
  invokeCalls: FakeInvokeCall[];
  invokeHandler: FakeInvokeHandler;
  eventListeners: Map<string, Set<EventListener>>;
  nextEventId: number;
}

export interface FakeInvokeCall {
  cmd: string;
  args: Record<string, unknown>;
}

export type FakeInvokeHandler = (
  call: FakeInvokeCall,
  index: number,
) => unknown | Promise<unknown>;

type EventListener = (event: { event: string; id: number; payload: unknown }) => void;

const workbenchTestState = vi.hoisted((): WorkbenchTestState => {
  return {
    invokeCalls: [],
    invokeHandler: (): unknown => undefined,
    eventListeners: new Map<string, Set<EventListener>>(),
    nextEventId: 1,
  };
});

export { workbenchTestState };

/* ---------------------------------------------------------------------------
 * vi.mock — 接管 client invoke / tauri event / 重型子组件
 *
 * 注意：所有 factory 闭包引用 hoisted `workbenchTestState`，由测试运行前完成初始化。
 * ------------------------------------------------------------------------- */

/**
 * Business Logic（为什么需要这个 mock）:
 *   characterization 测试接管全部 IPC；production 已大量使用 invokeDecoded，
 *   mock 必须同时导出 invoke 与 invokeDecoded，否则 worktree/session/file 列表永远失败。
 *
 * Code Logic（这个 mock 做什么）:
 *   invoke 走共享 handler + 调用日志；invokeDecoded 先 invoke 再 decoder.decode；
 *   ContractDecodeError 原样抛出；api.invoke / api.invokeDecoded 同步暴露。
 */
vi.mock('@/api/client', async () => {
  const { ContractDecodeError } = await import('@/lib/runtimeSchema');

  async function mockInvoke(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
    const call: FakeInvokeCall = { cmd, args: args ?? {} };
    workbenchTestState.invokeCalls.push(call);
    const sameCmdCount = workbenchTestState.invokeCalls.filter((c) => c.cmd === cmd).length;
    const result = workbenchTestState.invokeHandler(call, sameCmdCount - 1);
    try {
      return await result;
    } catch (error) {
      throw error instanceof Error ? error : new Error(String(error));
    }
  }

  async function mockInvokeDecoded<T>(
    command: string,
    args: Record<string, unknown> | undefined,
    decoder: { decode: (value: unknown, path?: string) => T },
  ): Promise<T> {
    const raw = await mockInvoke(command, args);
    try {
      return decoder.decode(raw, '$');
    } catch (reason) {
      if (reason instanceof ContractDecodeError) {
        throw reason;
      }
      throw reason;
    }
  }

  return {
    invoke: mockInvoke,
    invokeDecoded: mockInvokeDecoded,
    api: {
      invoke: mockInvoke,
      invokeDecoded: mockInvokeDecoded,
    },
  };
});

vi.mock('@tauri-apps/api/event', () => ({
  listen: async (
    event: string,
    handler: EventListener,
  ): Promise<() => void> => {
    workbenchTestState.nextEventId++;
    const set = workbenchTestState.eventListeners.get(event) ?? new Set<EventListener>();
    set.add(handler);
    workbenchTestState.eventListeners.set(event, set);
    return (): void => {
      set.delete(handler);
    };
  },
  once: async (
    event: string,
    handler: EventListener,
  ): Promise<() => void> => {
    workbenchTestState.nextEventId++;
    const wrapped: EventListener = (payload) => {
      handler(payload);
      workbenchTestState.eventListeners.get(event)?.delete(wrapped);
    };
    const set = workbenchTestState.eventListeners.get(event) ?? new Set<EventListener>();
    set.add(wrapped);
    workbenchTestState.eventListeners.set(event, set);
    return (): void => {
      set.delete(wrapped);
    };
  },
  emit: async (event: string, payload?: unknown): Promise<void> => {
    const set = workbenchTestState.eventListeners.get(event);
    if (!set) return;
    for (const handler of set) {
      handler({ event, id: 0, payload });
    }
  },
  emitTo: async (): Promise<void> => {
    /* 测试不依赖 emitTo */
  },
  TauriEvent: {},
}));

vi.mock('@xterm/xterm', () => {
  /**
   * xterm Terminal 桩：覆盖 Workbench 实际使用的全部方法（loadAddon/open/onData/onCursorMove/
   * attachCustomWheelEventHandler/onResize/dispose/buffer.active/write/options/getSelection），
   * 让 TerminalPane effect 在 jsdom 下也能完成挂载。
   * getSelection 必须存在：visible recovery 会在 selection 非空时跳过，缺方法会炸 characterization。
   */
  class TerminalMock {
    cols = 80;
    rows = 24;
    options: { theme?: unknown } = {};
    onResize = vi.fn(() => ({ dispose: () => undefined }));
    onData = vi.fn(() => ({ dispose: () => undefined }));
    onCursorMove = vi.fn(() => ({ dispose: () => undefined }));
    attachCustomWheelEventHandler = vi.fn();
    loadAddon = vi.fn();
    open = vi.fn();
    write = vi.fn();
    writeln = vi.fn();
    clear = vi.fn();
    reset = vi.fn();
    dispose = vi.fn();
    focus = vi.fn();
    scrollToBottom = vi.fn();
    getSelection = vi.fn(() => '');
    clearSelection = vi.fn();
    registerLinkProvider = vi.fn(() => ({ dispose: () => undefined }));
    modes = { mouseTrackingMode: 'none' };
    buffer = {
      active: {
        type: 'normal',
        baseY: 0,
        viewportY: 0,
        cursorX: 0,
        cursorY: 0,
        length: 0,
        getLine: () => null,
        base: { cursorX: 0, cursorY: 0, length: 0, getLine: () => null },
      },
    };
  }
  return { Terminal: TerminalMock };
});

vi.mock('@xterm/addon-fit', () => {
  class FitAddonMock {
    fit = vi.fn();
    proposeDimensions = vi.fn(() => ({ cols: 80, rows: 24 }));
    activate = vi.fn();
    dispose = vi.fn();
  }
  return { FitAddon: FitAddonMock };
});

vi.mock('@xterm/xterm/css/xterm.css', () => ({}));

vi.mock('@/pages/Orchestrator', () => ({
  OrchestratorPanel: (props: { embedded?: boolean; onOpenWorkbench?: (url: string) => void }) => (
    <div
      data-testid="orchestrator-panel"
      data-embedded={props.embedded ? 'true' : undefined}
      data-has-open-callback={props.onOpenWorkbench ? 'true' : undefined}
    >
      <button
        type="button"
        data-testid="orchestrator-open-workbench"
        onClick={() => props.onOpenWorkbench?.('/workbench?projectId=p1&worktreeId=wt-1&sessionId=s-1')}
      >
        open
      </button>
    </div>
  ),
  Orchestrator: () => <div data-testid="orchestrator" />,
}));

/**
 * Business Logic（为什么需要这些 mock）:
 *   Workbench.tsx 从 `@/components/domain/<Name>` 深路径导入重型子组件，
 *   必须在桶路径与深路径同时 mock，否则真实 CodeMirror/SessionSearch 会进入 jsdom。
 *
 * Code Logic（这些 mock 做什么）:
 *   factory 内联 stub（避免顶层组件函数触发 react-refresh only-export-components）；
 *   用 data-testid 暴露状态并提供回调触发按钮。
 */
vi.mock('@/components/domain', async () => {
  const actual = await vi.importActual<Record<string, unknown>>('@/components/domain');
  const WorkbenchFileWorkspace = (props: Record<string, unknown>) => (
    <div
      data-testid="workbench-file-workspace"
      data-active-tab-id={String(props.activeTabId ?? '')}
      data-saving={props.saving ? 'true' : undefined}
      data-write-disabled={props.writeDisabled ? 'true' : undefined}
      data-tab-count={Array.isArray(props.tabs) ? props.tabs.length : 0}
    >
      <button
        type="button"
        data-testid="workbench-file-workspace-return-terminal"
        onClick={() => (props.onReturnToTerminal as (() => void) | undefined)?.()}
      >
        return
      </button>
      <button
        type="button"
        data-testid="workbench-file-workspace-save"
        onClick={() =>
          (props.onSave as ((id: string) => void) | undefined)?.(String(props.activeTabId ?? ''))
        }
      >
        save
      </button>
      <button
        type="button"
        data-testid="workbench-file-workspace-format"
        onClick={() =>
          (props.onFormat as ((id: string) => void) | undefined)?.(String(props.activeTabId ?? ''))
        }
      >
        format
      </button>
      <button
        type="button"
        data-testid="workbench-file-workspace-close"
        onClick={() =>
          (props.onClose as ((id: string) => void) | undefined)?.(String(props.activeTabId ?? ''))
        }
      >
        close
      </button>
      <button
        type="button"
        data-testid="workbench-file-workspace-content-change"
        data-file-content-value={String(
          (Array.isArray(props.tabs)
            ? (props.tabs as Array<{ id?: string; content?: string }>).find(
                (tab) => tab.id === props.activeTabId,
              )?.content
            : undefined) ?? '',
        )}
        onClick={() =>
          (props.onContentChange as ((id: string, value: string) => void) | undefined)?.(
            String(props.activeTabId ?? ''),
            'edited-content',
          )
        }
      >
        edit
      </button>
    </div>
  );
  const WorkbenchBrowserWorkspace = (props: Record<string, unknown>) => (
    <div
      data-testid="workbench-browser-workspace"
      data-surface={String(props.surface ?? '')}
      data-project-id={String((props.project as { id?: string } | null)?.id ?? '')}
      data-worktree-id={String((props.worktree as { id?: string } | null)?.id ?? '')}
    >
      <button
        type="button"
        data-testid="workbench-browser-workspace-return"
        onClick={() => (props.onReturnToTerminal as (() => void) | undefined)?.()}
      >
        return
      </button>
    </div>
  );
  const WorkbenchSessionSearch = (props: {
    open?: boolean;
    offline?: boolean;
    projectId?: string | null;
    worktreeId?: string | null;
    onResumed?: (sessionId: string) => void;
    onClose?: () => void;
  }) => (
    <div
      data-testid="workbench-session-search"
      data-open={props.open ? 'true' : undefined}
      data-offline={props.offline ? 'true' : undefined}
      data-project-id={String(props.projectId ?? '')}
      data-worktree-id={String(props.worktreeId ?? '')}
    >
      <button
        type="button"
        data-testid="workbench-session-search-resume"
        onClick={() => props.onResumed?.('resumed-session')}
      >
        resume
      </button>
      <button type="button" data-testid="workbench-session-search-close" onClick={() => props.onClose?.()}>
        close
      </button>
    </div>
  );
  const WorkbenchDependencyCard = () => <div data-testid="workbench-dependency-card">dependency</div>;
  return {
    ...actual,
    WorkbenchFileWorkspace,
    WorkbenchBrowserWorkspace,
    WorkbenchSessionSearch,
    WorkbenchDependencyCard,
  };
});

// 深路径 stub：Workbench.tsx 不经桶路径 import；此处返回与桶 mock 等价的轻量桩。
vi.mock('@/components/domain/WorkbenchFileWorkspace', () => ({
  WorkbenchFileWorkspace: (props: Record<string, unknown>) => (
    <div
      data-testid="workbench-file-workspace"
      data-active-tab-id={String(props.activeTabId ?? '')}
      data-saving={props.saving ? 'true' : undefined}
      data-write-disabled={props.writeDisabled ? 'true' : undefined}
      data-tab-count={Array.isArray(props.tabs) ? props.tabs.length : 0}
    >
      <button
        type="button"
        data-testid="workbench-file-workspace-return-terminal"
        onClick={() => (props.onReturnToTerminal as (() => void) | undefined)?.()}
      >
        return
      </button>
      <button
        type="button"
        data-testid="workbench-file-workspace-save"
        onClick={() =>
          (props.onSave as ((id: string) => void) | undefined)?.(String(props.activeTabId ?? ''))
        }
      >
        save
      </button>
      <button
        type="button"
        data-testid="workbench-file-workspace-format"
        onClick={() =>
          (props.onFormat as ((id: string) => void) | undefined)?.(String(props.activeTabId ?? ''))
        }
      >
        format
      </button>
      <button
        type="button"
        data-testid="workbench-file-workspace-close"
        onClick={() =>
          (props.onClose as ((id: string) => void) | undefined)?.(String(props.activeTabId ?? ''))
        }
      >
        close
      </button>
      <button
        type="button"
        data-testid="workbench-file-workspace-content-change"
        data-file-content-value={String(
          (Array.isArray(props.tabs)
            ? (props.tabs as Array<{ id?: string; content?: string }>).find(
                (tab) => tab.id === props.activeTabId,
              )?.content
            : undefined) ?? '',
        )}
        onClick={() =>
          (props.onContentChange as ((id: string, value: string) => void) | undefined)?.(
            String(props.activeTabId ?? ''),
            'edited-content',
          )
        }
      >
        edit
      </button>
    </div>
  ),
}));
vi.mock('@/components/domain/WorkbenchBrowserWorkspace', () => ({
  WorkbenchBrowserWorkspace: (props: Record<string, unknown>) => (
    <div
      data-testid="workbench-browser-workspace"
      data-surface={String(props.surface ?? '')}
      data-project-id={String((props.project as { id?: string } | null)?.id ?? '')}
      data-worktree-id={String((props.worktree as { id?: string } | null)?.id ?? '')}
    >
      <button
        type="button"
        data-testid="workbench-browser-workspace-return"
        onClick={() => (props.onReturnToTerminal as (() => void) | undefined)?.()}
      >
        return
      </button>
    </div>
  ),
}));
vi.mock('@/components/domain/WorkbenchSessionSearch', () => ({
  WorkbenchSessionSearch: (props: {
    open?: boolean;
    offline?: boolean;
    projectId?: string | null;
    worktreeId?: string | null;
    onResumed?: (sessionId: string) => void;
    onClose?: () => void;
  }) => (
    <div
      data-testid="workbench-session-search"
      data-open={props.open ? 'true' : undefined}
      data-offline={props.offline ? 'true' : undefined}
      data-project-id={String(props.projectId ?? '')}
      data-worktree-id={String(props.worktreeId ?? '')}
    >
      <button
        type="button"
        data-testid="workbench-session-search-resume"
        onClick={() => props.onResumed?.('resumed-session')}
      >
        resume
      </button>
      <button type="button" data-testid="workbench-session-search-close" onClick={() => props.onClose?.()}>
        close
      </button>
    </div>
  ),
}));
vi.mock('@/components/domain/WorkbenchDependencyCard', () => ({
  WorkbenchDependencyCard: () => <div data-testid="workbench-dependency-card">dependency</div>,
}));

/* ---------------------------------------------------------------------------
 * Deferred helpers
 * ------------------------------------------------------------------------- */

/** 可控 deferred，便于延迟解析/拒绝。 */
export interface Deferred<T = unknown> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
  /** 已经 resolve/reject。 */
  settled: boolean;
}

export function createDeferred<T = unknown>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const state = { settled: false };
  const promise = new Promise<T>((res, rej) => {
    resolve = (value) => {
      state.settled = true;
      res(value);
    };
    reject = (error) => {
      state.settled = true;
      rej(error);
    };
  });
  return { promise, resolve, reject, get settled() {
    return state.settled;
  } };
}

/* ---------------------------------------------------------------------------
 * Default invoke handler + state reset helpers
 * ------------------------------------------------------------------------- */

/** 默认 invoke handler：返回测试数据兜底，避免页面渲染崩溃。 */
export function buildDefaultInvokeHandler(data: {
  projects?: WorkbenchProject[];
  activeProjectId?: string | null;
  sessions?: WorkbenchSession[];
  worktrees?: WorkbenchWorktree[];
  rootFileNodes?: WorkbenchFileNode[];
  gitCommits?: WorkbenchGitCommit[];
}): FakeInvokeHandler {
  const projects = data.projects ?? [];
  const activeProjectId = data.activeProjectId ?? projects[0]?.id ?? null;
  const worktrees = data.worktrees ?? [];
  const sessions = data.sessions ?? [];
  const rootNodes = data.rootFileNodes ?? [];
  const gitCommits = data.gitCommits ?? [];

  return (call) => {
    switch (call.cmd) {
      case 'get_workbench_launch_summary':
        return {
          projects: { kind: 'ready', value: [] },
          sessions: { kind: 'ready', value: [] },
          tasks: { kind: 'ready', value: [] },
          transfers: { kind: 'ready', value: [] },
          generatedAt: '2026-07-14T00:00:00.000Z',
        };
      case 'list_workbench_projects':
        return projects;
      case 'list_workbench_worktrees':
        return activeProjectId ? worktrees : [];
      case 'list_workbench_sessions':
        return activeProjectId ? sessions : [];
      case 'list_workbench_git_commits':
        return gitCommits;
      case 'list_workbench_dir':
        return rootNodes;
      case 'get_workbench_path_info': {
        const path = (call.args.path as string) ?? '';
        const node = rootNodes.find((n) => n.path === path);
        return (
          node ?? {
            name: path.split('/').pop() ?? '',
            path,
            kind: 'file',
            size: 0,
            modifiedAt: null,
          }
        );
      }
      case 'open_workbench_file': {
        // open_workbench_file 现经 invokeDecoded；返回合法 WorkbenchOpenFile 形状，避免 ContractDecodeError。
        const path = (call.args.path as string) ?? 'README.md';
        const node = rootNodes.find((n) => n.path === path);
        const metadata = node
          ? {
              name: node.name,
              path: node.path,
              kind: node.kind,
              size: node.size,
              modifiedAt: node.modifiedAt,
            }
          : {
              name: path.split('/').pop() ?? path,
              path,
              kind: 'file' as const,
              size: 12,
              modifiedAt: '2026-07-01T00:00:00.000Z',
            };
        return {
          metadata,
          detectedType: 'markdown',
          capabilities: {
            canPreview: true,
            canEdit: true,
            canFormat: false,
            mustValidateBeforeSave: false,
            defaultMode: 'edit',
            availableModes: ['edit', 'preview'],
          },
          text: {
            content: '# hello\n',
            baseHash: 'hash-readme',
            baseModifiedAt: metadata.modifiedAt,
          },
          image: null,
          csv: null,
          sqlite: null,
          truncated: false,
          notice: null,
        };
      }
      case 'touch_workbench_project':
        return projects.find((p) => p.id === call.args.projectId) ?? null;
      case 'get_workbench_dependency_status':
        return {
          status: 'ready',
          available: true,
          version: null,
          backend: 'native',
          path: null,
          installable: false,
          installCommandPreview: [],
          error: null,
          output: [],
          statusChangedAt: '2026-07-12T00:00:00.000Z',
        };
      case 'get_config':
        return {
          deviceName: 'device',
          receiveDir: '',
          gamePluginDir: '',
          screenshotHotkey: '<ctrl>',
          promptOptimizerHotkey: '<ctrl>',
          promptOptimizerFillLanguage: 'zh' satisfies PromptOptimizerFillLanguage,
        };
      default:
        return { ok: true };
    }
  };
}

/** 重置共享测试状态：清空调用日志、事件监听器，并装回默认 handler。 */
export function resetWorkbenchTestState(): void {
  workbenchTestState.invokeCalls.length = 0;
  workbenchTestState.invokeHandler = (): unknown => undefined;
  workbenchTestState.eventListeners.clear();
  workbenchTestState.nextEventId = 1;
  // 默认锁定中文文案，避免 navigator.language 不可控影响断言。
  try {
    window.localStorage.setItem('cp-lang', 'zh');
  } catch {
    /* localStorage 不可用时忽略 */
  }
  void i18n.changeLanguage('zh');
}

/** 安装新的 invoke handler。 */
export function setInvokeHandler(handler: FakeInvokeHandler): void {
  workbenchTestState.invokeHandler = handler;
}

/** 取某命令的全部调用记录。 */
export function invokeCallsFor(cmd: string): FakeInvokeCall[] {
  return workbenchTestState.invokeCalls.filter((call) => call.cmd === cmd);
}

/** 触发一个 Tauri 事件，所有同事件名监听器都会收到。 */
export function emitWorkbenchEvent(event: string, payload: unknown): void {
  const set = workbenchTestState.eventListeners.get(event);
  if (!set) return;
  for (const handler of [...set]) {
    handler({ event, id: 0, payload });
  }
}

/** 当前已注册的某事件名监听器数量（用于断言 listen 是否生效）。 */
export function workbenchEventListenerCount(event: string): number {
  return workbenchTestState.eventListeners.get(event)?.size ?? 0;
}

/**
 * 点击 inspector tab 切换到 files / history。
 *
 * Business Logic（为什么需要这个函数）:
 *   inspector 默认 tab 是产品决策（当前默认 Git 历史），不应被领域 characterization 测试隐式锁定。
 *   文件域、dirty file tab 等测试显式切到所需 tab，避免默认值变更波及领域行为断言。
 *
 * Code Logic（这个函数做什么）:
 *   按 WorkbenchInspector 暴露的稳定 button id（workbench-inspector-tab-<tab>）点击对应 tab。
 */
export async function selectInspectorTab(tab: 'files' | 'history' | 'notes'): Promise<void> {
  await act(async () => {
    const el = document.getElementById(`workbench-inspector-tab-${tab}`);
    if (!el) {
      throw new Error(`inspector tab "${tab}" button not found`);
    }
    fireEvent.click(el);
  });
}

/* ---------------------------------------------------------------------------
 * Project / dependency context stubs
 * ------------------------------------------------------------------------- */

export function buildProjectsContextValue(
  data: {
    projects?: WorkbenchProject[];
    activeProjectId?: string | null;
  },
  overrides: Partial<WorkbenchProjectsContextValue> = {},
): WorkbenchProjectsContextValue {
  const projects = data.projects ?? [];
  // 显式传 null 表示「有项目但未选中」启动表面，不能用 ?? 回落到 projects[0]。
  const activeProjectId =
    data.activeProjectId !== undefined ? data.activeProjectId : projects[0]?.id ?? null;
  const activeProject = projects.find((p) => p.id === activeProjectId) ?? null;
  return {
    projects,
    activeProjectId,
    activeProject,
    projectsLoading: false,
    projectBusy: false,
    projectError: null,
    projectSessionStats: {} as Record<string, WorkbenchProjectSessionStats>,
    loadProjects: vi.fn(async () => Promise.resolve()),
    refreshProjectSessionStats: vi.fn(async () => Promise.resolve()),
    chooseAndAddProject: vi.fn(async () => Promise.resolve(null)),
    openRemoteProject: vi.fn(async () => Promise.resolve(null)),
    selectProject: vi.fn(async (project: WorkbenchProject) => Promise.resolve(project)),
    removeProject: vi.fn(async () => Promise.resolve()),
    reorderProjects: vi.fn(async () => Promise.resolve()),
    currentWindowLabel: 'main',
    occupancy: [],
    openProjectInNewWindow: vi.fn(async () => Promise.resolve()),
    ...overrides,
  };
}

export function buildDependencyContextValue(
  overrides: Partial<WorkbenchDependencyContextValue> = {},
): WorkbenchDependencyContextValue {
  return {
    status: {
      status: 'ready',
      available: true,
      version: null,
      backend: 'native',
      path: null,
      installable: false,
      installCommandPreview: [],
      error: null,
      output: [],
      statusChangedAt: '2026-07-12T00:00:00.000Z',
    },
    checking: false,
    installing: false,
    error: null,
    check: vi.fn(async () => Promise.resolve()),
    install: vi.fn(async () => Promise.resolve()),
    cancel: vi.fn(async () => Promise.resolve()),
    ...overrides,
  };
}

/* ---------------------------------------------------------------------------
 * Sample fixture builders
 * ------------------------------------------------------------------------- */

export function buildLocalProject(overrides: Partial<WorkbenchProject> = {}): WorkbenchProject {
  return {
    id: 'project-1',
    name: 'demo-project',
    kind: 'local',
    deviceId: 'device-local',
    deviceName: 'MacBook',
    path: '/Users/demo/project',
    lastOpenedAt: '2026-07-01T00:00:00.000Z',
    createdAt: '2026-01-01T00:00:00.000Z',
    updatedAt: '2026-07-01T00:00:00.000Z',
    ...overrides,
  };
}

export function buildRemoteProject(overrides: Partial<WorkbenchProject> = {}): WorkbenchProject {
  return {
    id: 'project-remote',
    name: 'remote-project',
    kind: 'remote',
    deviceId: 'device-remote',
    deviceName: 'Remote Pi',
    path: '/home/demo/project',
    lastOpenedAt: '2026-07-01T00:00:00.000Z',
    createdAt: '2026-01-01T00:00:00.000Z',
    updatedAt: '2026-07-01T00:00:00.000Z',
    ...overrides,
  };
}

export function buildWorktree(overrides: Partial<WorkbenchWorktree> = {}): WorkbenchWorktree {
  return {
    id: 'worktree-main',
    projectId: 'project-1',
    name: 'main',
    branch: 'main',
    baseBranch: null,
    path: '/Users/demo/project',
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
    createdAt: '2026-01-01T00:00:00.000Z',
    updatedAt: '2026-07-01T00:00:00.000Z',
    ...overrides,
  };
}

export function buildSession(overrides: Partial<WorkbenchSession> = {}): WorkbenchSession {
  return {
    id: 'session-1',
    projectId: 'project-1',
    worktreeId: 'worktree-main',
    name: 'main terminal',
    command: 'bash',
    cwd: '/Users/demo/project',
    status: 'running',
    cols: 80,
    rows: 24,
    startedAt: '2026-07-01T00:00:00.000Z',
    exitedAt: null,
    exitCode: null,
    supportsPanes: true,
    paneCount: 1,
    ...overrides,
  };
}

export function buildFileNode(overrides: Partial<WorkbenchFileNode> = {}): WorkbenchFileNode {
  return {
    name: 'README.md',
    path: 'README.md',
    kind: 'file',
    size: 12,
    modifiedAt: '2026-07-01T00:00:00.000Z',
    children: null,
    ...overrides,
  };
}

export function buildPathInfo(overrides: Partial<WorkbenchPathInfo> = {}): WorkbenchPathInfo {
  return {
    name: 'README.md',
    path: 'README.md',
    kind: 'file',
    size: 12,
    modifiedAt: '2026-07-01T00:00:00.000Z',
    ...overrides,
  };
}

/* ---------------------------------------------------------------------------
 * render helper
 * ------------------------------------------------------------------------- */

export interface RenderWorkbenchOptions {
  initialSearch?: string;
  initialPath?: string;
}

export interface RenderedWorkbench {
  container: HTMLElement;
  rerender: (ui: ReactElement) => void;
  unmount: () => void;
  user: ReturnType<typeof userEvent.setup>;
  /** 运行时切换 Projects context value，触发 Workbench 重新读取 activeProjectId 等。 */
  setProjectsContext: (next: WorkbenchProjectsContextValue) => void;
  /** 运行时切换 Dependency context value。 */
  setDependencyContext: (next: WorkbenchDependencyContextValue) => void;
}

/**
 * 在 MemoryRouter + 项目/依赖/终端 buffer context 中渲染 Workbench。
 *
 * Business Logic: 真实 App 通过多层 Provider 包装 Workbench；本 helper 复刻相同层级，但用 stub context，
 * 让测试可以稳定控制项目、依赖与终端 buffer 状态。测试运行中需要切换 activeProjectId / activeWorktreeId 时，
 * 用 `setProjectsContext` 重置 context value 即可——这复刻了真实侧栏选择项目后页面收到的 context 变化。
 *
 * Code Logic: 用一个内部 stateful 包装组件持有 projects/dependency value，rerender 时不重建 Workbench，
 * 保留 ref/effect 连续性，便于断言 stale guard、focus 同步等时序行为。
 */

const emptyAttentionSnapshot = async (): Promise<AttentionSnapshot> => ({
  generatedAt: '2026-07-11T00:00:00.000Z',
  counts: { total: 0, decision: 0, blocked: 0, environment: 0 },
  items: [],
});

export function renderWorkbench(
  projectsValue: WorkbenchProjectsContextValue,
  dependencyValue: WorkbenchDependencyContextValue,
  options: RenderWorkbenchOptions = {},
): RenderedWorkbench {
  // 包装组件：把 context value 放进 React state，setProjectsContext/setDependencyContext 直接 setState。
  function HarnessRoot(): ReactElement {
    const [projects, setProjects] = useState<WorkbenchProjectsContextValue>(projectsValue);
    const [dependency, setDependency] = useState<WorkbenchDependencyContextValue>(dependencyValue);
    // 把 setter 暴露到外层闭包，render 后由 setProjectsContext 调用。
    setProjectsRef.current = setProjects;
    setDependencyRef.current = setDependency;
    return (
      <MemoryRouter
        initialEntries={[
          `${options.initialPath ?? '/workbench'}${options.initialSearch ?? ''}`,
        ]}
      >
        <WorkbenchProjectsContext.Provider value={projects}>
          <WorkbenchDependencyContext.Provider value={dependency}>
            <WorkbenchTerminalBuffersProvider>
              <AttentionProvider loadSnapshot={emptyAttentionSnapshot}>
                <Workbench />
              </AttentionProvider>
            </WorkbenchTerminalBuffersProvider>
          </WorkbenchDependencyContext.Provider>
        </WorkbenchProjectsContext.Provider>
      </MemoryRouter>
    );
  }

  const setProjectsRef: {
    current: ((next: WorkbenchProjectsContextValue) => void) | null;
  } = { current: null };
  const setDependencyRef: {
    current: ((next: WorkbenchDependencyContextValue) => void) | null;
  } = { current: null };

  let renderUtils: ReturnType<typeof render> | null = null;
  act(() => {
    renderUtils = render(<HarnessRoot />);
  });
  // Business Logic: TS 无法跨 act 闭包收窄 renderUtils 的非空类型，这里显式断言以便下方 return 取字段。
  const utils = renderUtils as ReturnType<typeof render> | null;
  if (!utils) throw new Error('renderWorkbench: render did not return');

  const setProjectsContext = (next: WorkbenchProjectsContextValue): void => {
    act(() => {
      setProjectsRef.current?.(next);
    });
  };
  const setDependencyContext = (next: WorkbenchDependencyContextValue): void => {
    act(() => {
      setDependencyRef.current?.(next);
    });
  };

  return {
    container: utils.container,
    rerender: utils.rerender,
    unmount: utils.unmount,
    user: userEvent.setup(),
    setProjectsContext,
    setDependencyContext,
  };
}

/** 等待所有挂起的 microtask（queueMicrotask、Promise.then、deferred.resolve 等）落地。 */
export async function flushMicrotasks(): Promise<void> {
  await act(async () => {
    for (let i = 0; i < 8; i += 1) {
      await Promise.resolve();
    }
  });
}

/** 等待至少 N 个 macrotask（setTimeout(0)），覆盖 deferEffect / setInterval 初始化。 */
export async function flushMacrotasks(rounds = 6): Promise<void> {
  await act(async () => {
    for (let i = 0; i < rounds; i += 1) {
      await new Promise<void>((resolve) => setTimeout(resolve, 0));
      await Promise.resolve();
    }
  });
}

/**
 * 轮询断言条件成立，最多等待 timeoutMs。
 *
 * Business Logic: Workbench 的 effect 链条很长（loadWorktrees → setActiveWorktreeId → scopedSessions →
 * deferEffect → setActiveSessionId → focus effect），固定 macrotask 轮数难以稳定；用轮询 + act 包裹最稳健。
 *
 * Code Logic: 每 ~16ms 检查一次 predicate，命中即返回；超时则抛出最后一次 predicate 的错误。
 */
export async function waitFor<T>(
  predicate: () => T | Promise<T>,
  options: { timeoutMs?: number; intervalMs?: number } = {},
): Promise<T> {
  const timeoutMs = options.timeoutMs ?? 2000;
  const intervalMs = options.intervalMs ?? 16;
  const start = Date.now();
  let lastError: unknown = undefined;
  while (Date.now() - start < timeoutMs) {
    try {
      const result = await predicate();
      return result;
    } catch (error) {
      lastError = error;
    }
    await act(async () => {
      await new Promise<void>((resolve) => setTimeout(resolve, intervalMs));
    });
  }
  throw lastError ?? new Error(`waitFor timed out after ${timeoutMs}ms`);
}

/** 等到某 invoke 命令被调用至少 expectedCount 次。 */
export async function waitForInvoke(
  cmd: string,
  expectedCount = 1,
): Promise<void> {
  await waitFor(() => {
    const count = invokeCallsFor(cmd).length;
    if (count < expectedCount) {
      throw new Error(`expected ${cmd} to be called >= ${expectedCount} times, got ${count}`);
    }
  });
}

/** 在 window 上安装 __TAURI_INTERNALS__.transformCallback，让 Workbench 的 listen effect 生效。 */
export function installTauriInternals(): void {
  const w = window as unknown as { __TAURI_INTERNALS__?: { transformCallback?: unknown } };
  if (!w.__TAURI_INTERNALS__) {
    w.__TAURI_INTERNALS__ = {};
  }
  w.__TAURI_INTERNALS__.transformCallback = (): number => 0;
}

/** 移除 transformCallback，模拟普通浏览器调试环境。 */
export function removeTauriInternals(): void {
  const w = window as unknown as { __TAURI_INTERNALS__?: { transformCallback?: unknown } };
  if (w.__TAURI_INTERNALS__) {
    delete w.__TAURI_INTERNALS__.transformCallback;
  }
}

/* ---------------------------------------------------------------------------
 * Harness sanity check（保证 import 本模块即生效，不污染其他测试文件）
 *
 * Business Logic: vitest 在收集 `describe`/`test` 时执行模块求值；此处放一个空 sanity test，
 * 确保 `vi.mock` factory 已被 hoist 注册、共享 state 已初始化，避免首个真实测试因 mock 未就绪而失败。
 *
 * Code Logic: 该 sanity test 只断言关键导出存在；每个 import 本模块的测试文件都会运行一次（开销可忽略）。
 * ------------------------------------------------------------------------- */
describe('workbenchTestHarness (sanity)', () => {
  test('exposes render + state helpers', () => {
    expect(typeof renderWorkbench).toBe('function');
    expect(typeof setInvokeHandler).toBe('function');
    expect(typeof emitWorkbenchEvent).toBe('function');
    expect(workbenchTestState.invokeCalls).toEqual([]);
  });
});

/* ---------------------------------------------------------------------------
 * 全局 afterEach：每个测试结束后清理 DOM、重置共享状态、移除 Tauri internals。
 *
 * Business Logic: 项目配置 `globals: false` 且没有 setupFiles，Testing Library 的自动 cleanup 不会触发，
 * 必须显式注册；否则跨用例的 Workbench 实例会泄漏，pending 异步 effect 在 reset 后读到 undefined handler 报错。
 *
 * Code Logic: 本模块被任一 characterization 测试 import 时，afterEffect 钩子即注册到该测试文件作用域。
 * ------------------------------------------------------------------------- */
afterEach(async () => {
  await flushMicrotasks();
  cleanup();
  resetWorkbenchTestState();
  removeTauriInternals();
});

/** 重新导出常用工具，便于测试文件单一 import。 */
export {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  within,
  userEvent,
  vi,
  expect,
  test,
  describe,
  afterEach,
};
export type { WorkbenchProjectsContextValue, WorkbenchDependencyContextValue };
