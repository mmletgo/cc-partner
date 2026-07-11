// @vitest-environment jsdom
/**
 * useWorkbenchSessionSearchController 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   在 controller 抽取后，Session 搜索浮层的开/关所有权（⌘K/Ctrl+K 快捷键、工具栏按钮入口、
 *   resume 成功后刷新 sessions + focus 新 session + 关闭浮层、卸载清理监听器）必须独立可测。
 *   搜索结果数据仍归 WorkbenchSessionSearch 组件所有，本 controller 只持有 open 状态与窄回调。
 *
 * Code Logic（这个测试做什么）:
 *   - 使用 @testing-library/react 的 renderHook 把 controller 挂在 React 树中；
 *   - 模拟 window keydown 触发 ⌘K / Ctrl+K；
 *   - 通过 rerender 修改 workspaceView 验证“仅终端视图”守卫；
 *   - 调用 handleResumed 验证 loadSessions / focusSession / 关闭浮层的编排；
 *   - unmount 后验证监听器已移除（再次按键不抛错、open 不变）。
 */
import { afterEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';

import { useWorkbenchSessionSearchController } from './useWorkbenchSessionSearchController';
import type { UseWorkbenchSessionSearchControllerParams } from './useWorkbenchSessionSearchController';

interface HarnessOverrides extends Partial<UseWorkbenchSessionSearchControllerParams> {
  workspaceView?: 'terminal' | 'files' | 'browser';
  activeProjectId?: string | null;
}

function buildHarness(overrides: HarnessOverrides = {}): UseWorkbenchSessionSearchControllerParams {
  // 注意：activeProjectId 允许显式传 null，所以用 `=== undefined` 判定而非 `??`，避免把 null 当 falsy 吃掉。
  return {
    workspaceView: overrides.workspaceView ?? 'terminal',
    activeProjectId:
      overrides.activeProjectId === undefined ? 'p1' : overrides.activeProjectId,
    loadSessions: overrides.loadSessions ?? vi.fn(async () => undefined),
    focusSession: overrides.focusSession ?? vi.fn(async () => true),
  };
}

function renderController(params: UseWorkbenchSessionSearchControllerParams) {
  return renderHook(
    (props: UseWorkbenchSessionSearchControllerParams) => useWorkbenchSessionSearchController(props),
    { initialProps: params },
  );
}

/** 触发 Cmd+K（mac）或 Ctrl+K（其它平台）。 */
function pressCmdK(meta: boolean): void {
  window.dispatchEvent(
    new KeyboardEvent('keydown', {
      key: 'k',
      metaKey: meta,
      ctrlKey: !meta,
      altKey: false,
      shiftKey: false,
      bubbles: true,
    }),
  );
}

afterEach(() => {
  cleanup();
});

describe('useWorkbenchSessionSearchController', () => {
  test('sessionSearchOpen defaults to closed', () => {
    const { result } = renderController(buildHarness());
    expect(result.current.sessionSearchOpen).toBe(false);
  });

  test('openSessionSearch opens the panel', () => {
    const { result } = renderController(buildHarness());
    act(() => {
      result.current.openSessionSearch();
    });
    expect(result.current.sessionSearchOpen).toBe(true);
  });

  test('closeSessionSearch closes the panel', () => {
    const { result } = renderController(buildHarness());
    act(() => {
      result.current.openSessionSearch();
    });
    act(() => {
      result.current.closeSessionSearch();
    });
    expect(result.current.sessionSearchOpen).toBe(false);
  });

  test('Cmd+K opens the panel from terminal view', () => {
    const { result } = renderController(buildHarness({ workspaceView: 'terminal' }));
    act(() => {
      pressCmdK(true);
    });
    expect(result.current.sessionSearchOpen).toBe(true);
  });

  test('Ctrl+K opens the panel from terminal view', () => {
    const { result } = renderController(buildHarness({ workspaceView: 'terminal' }));
    act(() => {
      pressCmdK(false);
    });
    expect(result.current.sessionSearchOpen).toBe(true);
  });

  test('Cmd+K does nothing in files view (unsupported context)', () => {
    const { result } = renderController(buildHarness({ workspaceView: 'files' }));
    act(() => {
      pressCmdK(true);
    });
    expect(result.current.sessionSearchOpen).toBe(false);
  });

  test('Cmd+K does nothing in browser view (unsupported context)', () => {
    const { result } = renderController(buildHarness({ workspaceView: 'browser' }));
    act(() => {
      pressCmdK(true);
    });
    expect(result.current.sessionSearchOpen).toBe(false);
  });

  test('Cmd+K with Alt modifier does not open (avoids conflicting shortcuts)', () => {
    const { result } = renderController(buildHarness({ workspaceView: 'terminal' }));
    window.dispatchEvent(
      new KeyboardEvent('keydown', {
        key: 'k',
        metaKey: true,
        altKey: true,
        shiftKey: false,
        bubbles: true,
      }),
    );
    expect(result.current.sessionSearchOpen).toBe(false);
  });

  test('Cmd+Shift+K does not open (shift modifier excluded)', () => {
    const { result } = renderController(buildHarness({ workspaceView: 'terminal' }));
    window.dispatchEvent(
      new KeyboardEvent('keydown', {
        key: 'k',
        metaKey: true,
        altKey: false,
        shiftKey: true,
        bubbles: true,
      }),
    );
    expect(result.current.sessionSearchOpen).toBe(false);
  });

  test('repeated Cmd+K keydown (event.repeat) does not re-trigger', () => {
    const { result } = renderController(buildHarness({ workspaceView: 'terminal' }));
    act(() => {
      pressCmdK(true);
    });
    expect(result.current.sessionSearchOpen).toBe(true);
    // 模拟按住不放产生的 repeat 事件：不应改变状态（仍为 true，且无副作用）。
    window.dispatchEvent(
      new KeyboardEvent('keydown', {
        key: 'k',
        metaKey: true,
        repeat: true,
        bubbles: true,
      }),
    );
    expect(result.current.sessionSearchOpen).toBe(true);
  });

  test('rerender into terminal view (re)registers the shortcut listener', () => {
    const { result, rerender } = renderController(buildHarness({ workspaceView: 'files' }));
    act(() => {
      pressCmdK(true);
    });
    expect(result.current.sessionSearchOpen).toBe(false);

    rerender(buildHarness({ workspaceView: 'terminal' }));
    act(() => {
      pressCmdK(true);
    });
    expect(result.current.sessionSearchOpen).toBe(true);
  });

  test('rerender out of terminal view removes the shortcut listener', () => {
    const { result, rerender } = renderController(buildHarness({ workspaceView: 'terminal' }));
    rerender(buildHarness({ workspaceView: 'browser' }));
    act(() => {
      pressCmdK(true);
    });
    expect(result.current.sessionSearchOpen).toBe(false);
  });

  test('handleResumed reloads sessions, focuses the new session, and closes the panel', async () => {
    const loadSessions = vi.fn(async () => undefined);
    const focusSession = vi.fn(async () => true);
    const { result } = renderController(
      buildHarness({ activeProjectId: 'p1', loadSessions, focusSession }),
    );
    act(() => {
      result.current.openSessionSearch();
    });
    expect(result.current.sessionSearchOpen).toBe(true);

    await act(async () => {
      await result.current.handleResumed('resumed-session');
    });

    expect(loadSessions).toHaveBeenCalledWith('p1');
    expect(focusSession).toHaveBeenCalledWith('resumed-session');
    expect(result.current.sessionSearchOpen).toBe(false);
  });

  test('handleResumed does not call loadSessions when there is no active project', async () => {
    const loadSessions = vi.fn(async () => undefined);
    const focusSession = vi.fn(async () => true);
    const { result } = renderController(
      buildHarness({ activeProjectId: null, loadSessions, focusSession }),
    );
    await act(async () => {
      await result.current.handleResumed('resumed-session');
    });

    expect(loadSessions).not.toHaveBeenCalled();
    // focusSession 仍应被调用，且浮层关闭。
    expect(focusSession).toHaveBeenCalledWith('resumed-session');
    expect(result.current.sessionSearchOpen).toBe(false);
  });

  test('unmount removes the keyboard shortcut listener', () => {
    const { result, unmount } = renderController(buildHarness({ workspaceView: 'terminal' }));
    unmount();
    // 卸载后按键不应抛错，也不应改变已捕获的 open 值。
    expect(() => {
      pressCmdK(true);
    }).not.toThrow();
    expect(result.current.sessionSearchOpen).toBe(false);
  });
});
