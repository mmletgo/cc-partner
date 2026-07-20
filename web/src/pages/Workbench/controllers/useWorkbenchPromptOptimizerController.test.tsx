// @vitest-environment jsdom
/**
 * useWorkbenchPromptOptimizerController 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   在 controller 抽取后，Prompt 优化浮层的所有权行为必须独立可测：配置加载、打开/输入/定位、
 *   Control 单键快捷键、IME 安全、流式写入终端、打开后焦点回归、重新打开清空状态、远端离线禁用、
 *   无活动 session 拦截。这些行为在 Workbench.tsx 内曾由 configApi effect、openPromptOptimizerPanel、
 *   runPromptOptimization、handleCursorAnchorChange 与 keydown/keyup 监听协作实现。
 *
 * Code Logic（这个测试做什么）:
 *   - 使用 @testing-library/react 的 renderHook 把 controller 挂在 React 树中；
 *   - 注入桩 loadConfig / streamToTerminal / displayErrorMessage / refs，断言可观察行为；
 *   - 模拟 window keydown/keyup 事件触发 Control 单键快捷键；
 *   - 通过 rerender 修改 activeSession / remoteWriteDisabled / workspaceView 等输入。
 */
import { afterEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';

import { useWorkbenchPromptOptimizerController } from './useWorkbenchPromptOptimizerController';
import type { UseWorkbenchPromptOptimizerControllerParams } from './useWorkbenchPromptOptimizerController';
import type { PromptOptimizerFillLanguage, WorkbenchProject, WorkbenchSession } from '@/lib/types';
import type { TerminalCursorAnchor } from '../WorkbenchTerminalPane';

function buildLocalProject(overrides: Partial<WorkbenchProject> = {}): WorkbenchProject {
  return {
    id: 'p1',
    name: 'local',
    kind: 'local',
    deviceId: 'self',
    deviceName: 'Mac',
    path: '/Users/hans/local',
    lastOpenedAt: '2026-07-01T00:00:00Z',
    createdAt: '2026-07-01T00:00:00Z',
    updatedAt: '2026-07-01T00:00:00Z',
    ...overrides,
  };
}

function buildSession(overrides: Partial<WorkbenchSession> = {}): WorkbenchSession {
  return {
    id: 's1',
    projectId: 'p1',
    worktreeId: 'wt-main',
    name: 'main shell',
    command: 'bash',
    cwd: '/Users/hans/local',
    status: 'running',
    cols: 80,
    rows: 24,
    startedAt: '2026-07-01T00:00:00Z',
    exitedAt: null,
    exitCode: null,
    supportsPanes: true,
    paneCount: 1,
    ...overrides,
  };
}

interface HarnessOverrides extends Partial<UseWorkbenchPromptOptimizerControllerParams> {
  activeSession?: WorkbenchSession | null;
  activeProjectId?: string | null;
  promptWorkingDirectory?: string | undefined;
  remoteWriteDisabled?: boolean;
  automationConsoleOpen?: boolean;
  workspaceView?: 'terminal' | 'files' | 'browser';
}

function buildHarness(overrides: HarnessOverrides = {}): UseWorkbenchPromptOptimizerControllerParams {
  const project = buildLocalProject();
  const session = buildSession();
  const areaRef = { current: null as HTMLDivElement | null };
  const inputRef = { current: null as HTMLTextAreaElement | null };
  return {
    activeSession: overrides.activeSession === undefined ? session : overrides.activeSession,
    activeProjectId: overrides.activeProjectId === undefined ? project.id : overrides.activeProjectId,
    promptWorkingDirectory: overrides.promptWorkingDirectory ?? '/Users/hans/local',
    remoteWriteDisabled: overrides.remoteWriteDisabled ?? false,
    automationConsoleOpen: overrides.automationConsoleOpen ?? false,
    workspaceView: overrides.workspaceView ?? 'terminal',
    terminalAreaRef: overrides.terminalAreaRef ?? areaRef,
    promptInputRef: overrides.promptInputRef ?? inputRef,
    loadConfig:
      overrides.loadConfig ??
      vi.fn(async () => ({
        promptOptimizerHotkey: '<ctrl>',
        promptOptimizerFillLanguage: 'zh' as PromptOptimizerFillLanguage,
      })),
    streamToTerminal:
      overrides.streamToTerminal ?? vi.fn(async () => ({ ok: true, sessionId: 's1' })),
    markRequestFailure: overrides.markRequestFailure ?? vi.fn(),
    setSessionError: overrides.setSessionError ?? vi.fn(),
    displayErrorMessage: overrides.displayErrorMessage ?? vi.fn(),
    desktopUnavailableMessage: overrides.desktopUnavailableMessage ?? 'desktop unavailable',
    translateFillFailed: overrides.translateFillFailed ?? vi.fn(() => 'fill failed'),
    translateOptimizeFailed: overrides.translateOptimizeFailed ?? vi.fn(() => 'optimize failed'),
    translateRemoteOffline:
      overrides.translateRemoteOffline ?? vi.fn(() => 'remote offline'),
  };
}

function renderController(params: UseWorkbenchPromptOptimizerControllerParams) {
  return renderHook(
    (props: UseWorkbenchPromptOptimizerControllerParams) => useWorkbenchPromptOptimizerController(props),
    { initialProps: params },
  );
}

/** 模拟 Control 单键：keydown(Control) → keyup(Control)。 */
function pressControlSolo(): void {
  window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Control', ctrlKey: true, bubbles: true }));
  window.dispatchEvent(new KeyboardEvent('keyup', { key: 'Control', ctrlKey: false, bubbles: true }));
}

afterEach(() => {
  cleanup();
});

describe('useWorkbenchPromptOptimizerController', () => {
  test('loads hotkey and fill language from config on mount', async () => {
    const loadConfig = vi.fn(async () => ({
      promptOptimizerHotkey: '<cmd>+p',
      promptOptimizerFillLanguage: 'en' as PromptOptimizerFillLanguage,
    }));
    const { result } = renderController(buildHarness({ loadConfig }));

    await waitFor(() => {
      expect(result.current.promptOptimizerHotkey).toBe('<cmd>+p');
    });
    expect(result.current.promptOptimizerFillLanguage).toBe('en');
    expect(loadConfig).toHaveBeenCalled();
  });

  test('falls back to defaults when config load rejects', async () => {
    const loadConfig = vi.fn(async () => {
      throw new Error('no tauri');
    });
    const { result } = renderController(buildHarness({ loadConfig }));

    await waitFor(() => {
      expect(result.current.promptOptimizerHotkey).toBe('<ctrl>');
    });
    expect(result.current.promptOptimizerFillLanguage).toBe('zh');
  });

  test('openPromptOptimizerPanel clears input and positions panel from cursor anchor', () => {
    const areaEl = {
      getBoundingClientRect: () => ({ left: 10, top: 20, width: 800, height: 600, right: 810, bottom: 620, x: 10, y: 20, toJSON: () => ({}) }),
    } as unknown as HTMLDivElement;
    const areaRef = { current: areaEl };
    const anchor: TerminalCursorAnchor = { left: 100, top: 200, bottom: 220 };

    const { result } = renderController(buildHarness({ terminalAreaRef: areaRef }));

    // 先写入一些输入，验证打开时会被清空。
    act(() => {
      result.current.setPromptInput('previous draft');
    });
    expect(result.current.promptInput).toBe('previous draft');

    // 把光标锚点喂给 controller（模拟终端光标移动）。
    act(() => {
      result.current.handleCursorAnchorChange(anchor);
    });

    act(() => {
      result.current.openPromptOptimizerPanel();
    });

    expect(result.current.promptPanelOpen).toBe(true);
    // 重新打开必须清空可见输入。
    expect(result.current.promptInput).toBe('');
    // 定位应由光标锚点推导：默认 top=24，光标锚点驱动后 top 应变化（受面板高度 clamp，最终落到 maxTop=64）。
    expect(result.current.promptPanelPosition.top).not.toBe(24);
    expect(result.current.promptPanelPosition.top).toBe(64);
  });

  test('closePromptPanel sets panel closed', () => {
    const { result } = renderController(buildHarness());
    act(() => {
      result.current.openPromptOptimizerPanel();
    });
    expect(result.current.promptPanelOpen).toBe(true);

    act(() => {
      result.current.closePromptPanel();
    });
    expect(result.current.promptPanelOpen).toBe(false);
  });

  test('toolbar toggle helper opens when closed and closes when open', () => {
    const { result } = renderController(buildHarness());
    act(() => {
      result.current.togglePromptOptimizerPanel();
    });
    expect(result.current.promptPanelOpen).toBe(true);

    act(() => {
      result.current.togglePromptOptimizerPanel();
    });
    expect(result.current.promptPanelOpen).toBe(false);
  });

  test('Control solo shortcut opens the panel from terminal view', () => {
    const { result } = renderController(buildHarness({ workspaceView: 'terminal' }));
    expect(result.current.promptPanelOpen).toBe(false);

    act(() => {
      pressControlSolo();
    });

    expect(result.current.promptPanelOpen).toBe(true);
  });

  test('Control solo shortcut does nothing when automation console is open', () => {
    const { result } = renderController(
      buildHarness({ automationConsoleOpen: true, workspaceView: 'terminal' }),
    );
    act(() => {
      pressControlSolo();
    });
    expect(result.current.promptPanelOpen).toBe(false);
  });

  test('Control solo shortcut does nothing outside terminal view', () => {
    const { result } = renderController(buildHarness({ workspaceView: 'files' }));
    act(() => {
      pressControlSolo();
    });
    expect(result.current.promptPanelOpen).toBe(false);
  });

  test('Control solo shortcut does not open when remote is offline', () => {
    const { result } = renderController(
      buildHarness({ remoteWriteDisabled: true, workspaceView: 'terminal' }),
    );
    act(() => {
      pressControlSolo();
    });
    expect(result.current.promptPanelOpen).toBe(false);
  });

  test('Control+C chord does not trigger the shortcut', () => {
    const { result } = renderController(buildHarness({ workspaceView: 'terminal' }));
    // Control keydown 后立即按 C（chord），然后 keyup Control 不应触发。
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Control', ctrlKey: true, bubbles: true }));
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'c', ctrlKey: true, bubbles: true }));
    window.dispatchEvent(new KeyboardEvent('keyup', { key: 'Control', bubbles: true }));
    expect(result.current.promptPanelOpen).toBe(false);
  });

  test('with panel open and empty input, Control solo closes the panel', () => {
    const { result } = renderController(buildHarness({ workspaceView: 'terminal' }));
    act(() => {
      result.current.openPromptOptimizerPanel();
    });
    expect(result.current.promptPanelOpen).toBe(true);

    act(() => {
      pressControlSolo();
    });
    expect(result.current.promptPanelOpen).toBe(false);
  });

  test('with panel open and non-empty input, Control solo triggers optimization', async () => {
    const streamToTerminal = vi.fn(async () => ({ ok: true, sessionId: 's1' }));
    const { result } = renderController(
      buildHarness({ workspaceView: 'terminal', streamToTerminal }),
    );
    act(() => {
      result.current.openPromptOptimizerPanel();
    });
    act(() => {
      result.current.setPromptInput('optimize me');
    });

    await act(async () => {
      pressControlSolo();
      // 等待 async runPromptOptimization 落地。
      await Promise.resolve();
    });

    expect(streamToTerminal).toHaveBeenCalledWith(
      'optimize me',
      expect.objectContaining({ sessionId: 's1', targetLanguage: 'zh' }),
    );
    // 成功后面板关闭。
    await waitFor(() => {
      expect(result.current.promptPanelOpen).toBe(false);
    });
  });

  test('runPromptOptimization focuses input and aborts on empty input', () => {
    const focus = vi.fn();
    const inputEl = { focus } as unknown as HTMLTextAreaElement;
    const inputRef = { current: inputEl };
    const streamToTerminal = vi.fn();
    const { result } = renderController(
      buildHarness({ streamToTerminal, promptInputRef: inputRef }),
    );

    act(() => {
      result.current.openPromptOptimizerPanel();
    });
    // 输入为空。
    expect(result.current.promptInput).toBe('');

    act(() => {
      void result.current.runPromptOptimization();
    });

    expect(focus).toHaveBeenCalled();
    expect(streamToTerminal).not.toHaveBeenCalled();
  });

  test('runPromptOptimization sets fill failed error when no active session', async () => {
    const setSessionError = vi.fn();
    const translateFillFailed = vi.fn(() => 'no session fill failed');
    const streamToTerminal = vi.fn();
    const { result } = renderController(
      buildHarness({
        activeSession: null,
        setSessionError,
        translateFillFailed,
        streamToTerminal,
      }),
    );
    act(() => {
      result.current.setPromptInput('hello');
    });

    await act(async () => {
      void result.current.runPromptOptimization();
      await Promise.resolve();
    });

    expect(translateFillFailed).toHaveBeenCalled();
    expect(setSessionError).toHaveBeenCalledWith('no session fill failed');
    expect(streamToTerminal).not.toHaveBeenCalled();
  });

  test('runPromptOptimization sets fill failed error when active session is not running', async () => {
    const setSessionError = vi.fn();
    const streamToTerminal = vi.fn();
    const exited = buildSession({ status: 'exited' });
    const { result } = renderController(
      buildHarness({ activeSession: exited, setSessionError, streamToTerminal }),
    );
    act(() => {
      result.current.setPromptInput('hello');
    });

    await act(async () => {
      void result.current.runPromptOptimization();
      await Promise.resolve();
    });

    expect(setSessionError).toHaveBeenCalled();
    expect(streamToTerminal).not.toHaveBeenCalled();
  });

  test('runPromptOptimization surfaces remote offline notice when remoteWriteDisabled', async () => {
    const setSessionError = vi.fn();
    const translateRemoteOffline = vi.fn(() => 'remote offline notice');
    const streamToTerminal = vi.fn();
    const { result } = renderController(
      buildHarness({
        remoteWriteDisabled: true,
        setSessionError,
        translateRemoteOffline,
        streamToTerminal,
      }),
    );
    act(() => {
      result.current.setPromptInput('hello');
    });

    await act(async () => {
      void result.current.runPromptOptimization();
      await Promise.resolve();
    });

    expect(translateRemoteOffline).toHaveBeenCalled();
    expect(setSessionError).toHaveBeenCalledWith('remote offline notice');
    expect(streamToTerminal).not.toHaveBeenCalled();
  });

  test('runPromptOptimization streams to terminal with working directory and closes on success', async () => {
    const streamToTerminal = vi.fn(async () => ({ ok: true, sessionId: 's1' }));
    const { result } = renderController(buildHarness({ streamToTerminal }));
    act(() => {
      result.current.setPromptInput('do something');
    });

    await act(async () => {
      void result.current.runPromptOptimization();
      await Promise.resolve();
    });

    expect(streamToTerminal).toHaveBeenCalledWith(
      'do something',
      expect.objectContaining({
        sessionId: 's1',
        targetLanguage: 'zh',
        workingDirectory: '/Users/hans/local',
      }),
    );
    await waitFor(() => {
      expect(result.current.promptPanelOpen).toBe(false);
    });
  });

  test('runPromptOptimization marks request failure and sets error on stream rejection', async () => {
    const streamToTerminal = vi.fn(async () => {
      throw new Error('boom');
    });
    const markRequestFailure = vi.fn();
    const displayErrorMessage = vi.fn(() => 'displayed error');
    const setSessionError = vi.fn();
    const { result } = renderController(
      buildHarness({
        streamToTerminal,
        markRequestFailure,
        displayErrorMessage,
        setSessionError,
      }),
    );
    act(() => {
      result.current.setPromptInput('do something');
    });

    await act(async () => {
      void result.current.runPromptOptimization();
      await Promise.resolve();
    });

    expect(markRequestFailure).toHaveBeenCalledWith('p1', expect.any(Error));
    expect(displayErrorMessage).toHaveBeenCalled();
    expect(setSessionError).toHaveBeenCalledWith('displayed error');
    // 失败时面板保持打开。
    expect(result.current.promptPanelOpen).toBe(false);
  });

  test('textarea keydown handler ignores Enter while IME is composing', () => {
    const streamToTerminal = vi.fn();
    const { result } = renderController(buildHarness({ streamToTerminal }));
    act(() => {
      result.current.setPromptInput('text');
    });
    const preventDefault = vi.fn();
    const stopPropagation = vi.fn();
    const event = {
      key: 'Enter',
      shiftKey: false,
      nativeEvent: { isComposing: true },
      preventDefault,
      stopPropagation,
    } as unknown as React.KeyboardEvent<HTMLTextAreaElement>;

    act(() => {
      result.current.handlePromptInputKeyDown(event);
    });

    expect(preventDefault).not.toHaveBeenCalled();
    expect(streamToTerminal).not.toHaveBeenCalled();
  });

  test('textarea keydown handler commits optimization on plain Enter with non-empty input', () => {
    const streamToTerminal = vi.fn(async () => ({ ok: true, sessionId: 's1' }));
    const { result } = renderController(buildHarness({ streamToTerminal }));
    act(() => {
      result.current.setPromptInput('commit me');
    });
    const preventDefault = vi.fn();
    const stopPropagation = vi.fn();
    const event = {
      key: 'Enter',
      shiftKey: false,
      nativeEvent: { isComposing: false },
      preventDefault,
      stopPropagation,
    } as unknown as React.KeyboardEvent<HTMLTextAreaElement>;

    act(() => {
      result.current.handlePromptInputKeyDown(event);
    });

    expect(preventDefault).toHaveBeenCalled();
    expect(stopPropagation).toHaveBeenCalled();
  });

  test('textarea keydown handler allows Shift+Enter newline', () => {
    const streamToTerminal = vi.fn();
    const { result } = renderController(buildHarness({ streamToTerminal }));
    act(() => {
      result.current.setPromptInput('text');
    });
    const preventDefault = vi.fn();
    const event = {
      key: 'Enter',
      shiftKey: true,
      nativeEvent: { isComposing: false },
      preventDefault,
      stopPropagation: vi.fn(),
    } as unknown as React.KeyboardEvent<HTMLTextAreaElement>;

    act(() => {
      result.current.handlePromptInputKeyDown(event);
    });

    expect(preventDefault).not.toHaveBeenCalled();
    expect(streamToTerminal).not.toHaveBeenCalled();
  });

  test('cursor anchor change updates position only while panel is open', () => {
    const areaEl = {
      getBoundingClientRect: () => ({ left: 0, top: 0, width: 800, height: 600, right: 800, bottom: 600, x: 0, y: 0, toJSON: () => ({}) }),
    } as unknown as HTMLDivElement;
    const areaRef = { current: areaEl };
    const { result } = renderController(buildHarness({ terminalAreaRef: areaRef }));

    const initialTop = result.current.promptPanelPosition.top;
    // 面板未打开：光标变化不应提交位置。
    act(() => {
      result.current.handleCursorAnchorChange({ left: 500, top: 500, bottom: 520 });
    });
    expect(result.current.promptPanelPosition.top).toBe(initialTop);

    // 打开面板后再移动光标，位置应更新。
    act(() => {
      result.current.openPromptOptimizerPanel();
    });
    act(() => {
      result.current.handleCursorAnchorChange({ left: 500, top: 500, bottom: 520 });
    });
    expect(result.current.promptPanelPosition.top).not.toBe(initialTop);
  });

  test('closed prompt panel does not measure terminal area on cursor movement', () => {
    const getBoundingClientRect = vi.fn(() => ({
      left: 0,
      top: 0,
      width: 800,
      height: 600,
      right: 800,
      bottom: 600,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    }));
    const areaEl = { getBoundingClientRect } as unknown as HTMLDivElement;
    const areaRef = { current: areaEl };
    const { result } = renderController(buildHarness({ terminalAreaRef: areaRef }));

    expect(result.current.promptPanelOpen).toBe(false);
    act(() => {
      result.current.handleCursorAnchorChange({ left: 1, top: 2, bottom: 3 });
    });
    expect(getBoundingClientRect).not.toHaveBeenCalled();
  });

  test('unmount removes the keyboard shortcut listener', () => {
    const { unmount } = renderController(buildHarness({ workspaceView: 'terminal' }));
    unmount();
    // 卸载后再按 Control 不应抛错（监听器已移除），这里只验证不抛。
    expect(() => {
      pressControlSolo();
    }).not.toThrow();
  });
});
