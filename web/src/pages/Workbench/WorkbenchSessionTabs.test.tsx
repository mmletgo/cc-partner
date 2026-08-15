// @vitest-environment jsdom
/**
 * WorkbenchSessionTabs 测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   terminal tab 必须正确展示 hint 计数（等待/停止）并支持双击重命名。
 *
 * Code Logic（这个测试做什么）:
 *   渲染含 hint 的 tab，断言 hint 摘要 aria-label / 文案与重命名交互。
 */

import { afterEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import i18n from 'i18next';
import { I18nextProvider, initReactI18next } from 'react-i18next';
import type { ReactElement } from 'react';

import type { WorkbenchSession } from '@/lib/types';
import { WorkbenchSessionTabs } from './WorkbenchSessionTabs';

const resources = {
  zh: {
    workbench: {
      terminalTabs: '终端',
      closeTerminal: '关闭',
      newSession: '新建',
      renameSession: '重命名会话',
      renameSessionHint: '双击重命名',
      sessionNamePlaceholder: '会话名称',
      agentHints: {
        dotAriaWaiting: '{{count}} 个窗口等待输入',
        dotAriaCompleted: '{{count}} 个窗口已停止',
        dotAriaBoth: '{{waiting}} 个窗口等待输入，{{completed}} 个窗口已停止',
        navSummary: '等待 {{waiting}} · 已停止 {{stopped}}',
      },
    },
  },
};

void i18n.use(initReactI18next).init({
  lng: 'zh',
  resources,
  interpolation: { escapeValue: false },
});

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需挂载 i18n 上下文。
 *
 * Code Logic（这个函数做什么）:
 *   用 I18nextProvider 包裹 children。
 */
function wrap(ui: ReactElement): ReactElement {
  return <I18nextProvider i18n={i18n}>{ui}</I18nextProvider>;
}

/**
 * Business Logic（为什么需要这个工厂）:
 *   tab 测试只需最小 session 字段。
 *
 * Code Logic（这个函数做什么）:
 *   返回 running session。
 */
function makeSession(id = 's1'): WorkbenchSession {
  return {
    id,
    projectId: 'p1',
    worktreeId: 'wt1',
    name: 'Term 1',
    status: 'running',
    command: 'zsh',
    cols: 80,
    rows: 24,
    startedAt: '2026-07-15T00:00:00.000Z',
    exitedAt: null,
    exitCode: null,
    cwd: '/tmp',
    supportsPanes: false,
    paneCount: 1,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   统一渲染 SessionTabs 并注入 hint map。
 *
 * Code Logic（这个函数做什么）:
 *   render + resolveHint。
 */
function renderSessionTab(options: {
  onFocusSession?: (id: string) => void;
  onRenameSession?: (sessionId: string, name: string) => Promise<boolean>;
  canRename?: boolean;
  resolveHint?: (sessionId: string) => {
    waitingCount: number;
    stoppedCount: number;
    completedCount: number;
    count: number;
    tone: 'wait' | 'complete' | 'zero';
  };
}): void {
  render(
    wrap(
      <WorkbenchSessionTabs
        sessions={[makeSession()]}
        activeSessionId="s1"
        sessionBusy={false}
        canCreate
        onFocusSession={options.onFocusSession ?? vi.fn()}
        onCloseSession={async () => undefined}
        onCreateSession={() => undefined}
        onRenameSession={options.onRenameSession ?? vi.fn().mockResolvedValue(true)}
        canRename={options.canRename ?? true}
        resolveHint={options.resolveHint}
      />,
    ),
  );
}

describe('WorkbenchSessionTabs hint summary', () => {
  afterEach(() => {
    cleanup();
  });

  test('sessionDot shows waiting count when resolveHint has waiting', () => {
    renderSessionTab({
      resolveHint: () => ({
        waitingCount: 1,
        stoppedCount: 0,
        completedCount: 0,
        count: 1,
        tone: 'wait',
      }),
    });
    const summary = screen.getByTestId('workbench-session-hint-summary');
    expect(summary.textContent).toBe('1/0');
    expect(screen.getByLabelText('等待 1 · 已停止 0')).toBeTruthy();
    const dots = document.querySelectorAll('[data-hint-tone="wait"]');
    expect(dots.length).toBeGreaterThan(1);
    expect(screen.getByLabelText('1 个窗口等待输入').textContent).toBe('1/0');
  });

  test('nav summary shows 0/0 when no waiting or stopped agents', () => {
    renderSessionTab({});
    const summary = screen.getByTestId('workbench-session-hint-summary');
    expect(summary.textContent).toBe('0/0');
    expect(summary.getAttribute('data-hint-tone')).toBe('zero');
    expect(screen.getByLabelText('等待 0 · 已停止 0')).toBeTruthy();
  });
});

describe('WorkbenchSessionTabs inline rename', () => {
  afterEach(() => {
    cleanup();
  });

  test('double-click name enters edit mode with prefilled value', () => {
    renderSessionTab({ onRenameSession: vi.fn() });
    fireEvent.dblClick(screen.getByText('Term 1'));
    const input = screen.getByLabelText('重命名会话') as HTMLInputElement;
    expect(input).toBeTruthy();
    expect(input.value).toBe('Term 1');
  });

  test('Enter commits the renamed value via onRenameSession exactly once', () => {
    const onRename = vi.fn().mockResolvedValue(true);
    renderSessionTab({ onRenameSession: onRename });
    fireEvent.dblClick(screen.getByText('Term 1'));
    const input = screen.getByLabelText('重命名会话') as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'Build' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(onRename).toHaveBeenCalledTimes(1);
    expect(onRename).toHaveBeenCalledWith('s1', 'Build');
  });

  test('Escape cancels without calling onRenameSession', () => {
    const onRename = vi.fn();
    renderSessionTab({ onRenameSession: onRename });
    fireEvent.dblClick(screen.getByText('Term 1'));
    const input = screen.getByLabelText('重命名会话') as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'Build' } });
    fireEvent.keyDown(input, { key: 'Escape' });
    expect(onRename).not.toHaveBeenCalled();
    expect(screen.queryByLabelText('重命名会话')).toBeNull();
  });

  test('empty / whitespace name on commit does not call onRenameSession', () => {
    const onRename = vi.fn();
    renderSessionTab({ onRenameSession: onRename });
    fireEvent.dblClick(screen.getByText('Term 1'));
    const input = screen.getByLabelText('重命名会话') as HTMLInputElement;
    fireEvent.change(input, { target: { value: '   ' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(onRename).not.toHaveBeenCalled();
  });

  test('unchanged name does not call onRenameSession', () => {
    const onRename = vi.fn();
    renderSessionTab({ onRenameSession: onRename });
    fireEvent.dblClick(screen.getByText('Term 1'));
    const input = screen.getByLabelText('重命名会话') as HTMLInputElement;
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(onRename).not.toHaveBeenCalled();
  });

  test('canRename=false blocks entering edit mode', () => {
    const onRename = vi.fn();
    renderSessionTab({ onRenameSession: onRename, canRename: false });
    fireEvent.dblClick(screen.getByText('Term 1'));
    expect(screen.queryByLabelText('重命名会话')).toBeNull();
    expect(onRename).not.toHaveBeenCalled();
  });

  test('blur commits the renamed value', () => {
    const onRename = vi.fn().mockResolvedValue(true);
    renderSessionTab({ onRenameSession: onRename });
    fireEvent.dblClick(screen.getByText('Term 1'));
    const input = screen.getByLabelText('重命名会话') as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'Logs' } });
    fireEvent.blur(input);
    expect(onRename).toHaveBeenCalledTimes(1);
    expect(onRename).toHaveBeenCalledWith('s1', 'Logs');
  });
});
