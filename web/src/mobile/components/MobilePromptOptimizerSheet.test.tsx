// @vitest-environment jsdom
/**
 * 移动端 Prompt 优化浮层成功提示自动消失。
 *
 * Business Logic（为什么需要这个测试）:
 *   写入终端成功只是短暂确认，不能一直钉在 bottom sheet 里。
 *
 * Code Logic（这个测试做什么）:
 *   mock streamToTerminal；提交成功后立刻看到「已开始写入」；推进 MOBILE_TRANSIENT_STATUS_MS 后消失。
 */

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import type { WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import { MOBILE_TRANSIENT_STATUS_MS } from '../mobileTransientStatus';

const streamToTerminalMock = vi.fn();

vi.mock('@/api/workbenchHttp', () => ({
  httpWorkbenchTransport: {
    prompt: {
      streamToTerminal: (...args: unknown[]) => streamToTerminalMock(...args),
    },
  },
}));

import { MobilePromptOptimizerSheet } from './MobilePromptOptimizerSheet';

function createWorktree(): WorkbenchWorktree {
  return {
    id: 'wt-1',
    projectId: 'project-1',
    name: 'feature/x',
    branch: 'feature/x',
    baseBranch: 'main',
    path: '/tmp/demo-feature',
    isMain: false,
    canCollectMerge: false,
    homeBranch: null,
    collectibleBranches: [],
    status: {
      branch: 'feature/x',
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

function createSession(): WorkbenchSession {
  return {
    id: 'session-1',
    projectId: 'project-1',
    worktreeId: 'wt-1',
    name: 'window-1',
    command: 'zsh',
    cwd: '/tmp/demo-feature',
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

function renderSheet(): void {
  render(
    <I18nextProvider i18n={i18n}>
      <MobilePromptOptimizerSheet
        open
        onClose={() => undefined}
        worktree={createWorktree()}
        session={createSession()}
      />
    </I18nextProvider>,
  );
}

describe('MobilePromptOptimizerSheet sent status', () => {
  beforeAll(async () => {
    await i18n.changeLanguage('zh');
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    streamToTerminalMock.mockReset();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   写入成功后的确认必须自动消失，否则挡住下一次输入。
   *
   * Code Logic（这个测试做什么）:
   *   填 Prompt 并提交；成功文案在 delay 前可见，到期后消失。
   */
  test('sent status auto-dismisses', async () => {
    streamToTerminalMock.mockResolvedValue({ ok: true, sessionId: 'session-1' });
    renderSheet();
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '优化这段 Prompt' } });
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    fireEvent.click(screen.getByRole('button', { name: '写入当前终端' }));
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(screen.getByText('已开始写入当前终端')).toBeTruthy();

    act(() => {
      vi.advanceTimersByTime(MOBILE_TRANSIENT_STATUS_MS - 1);
    });
    expect(screen.getByText('已开始写入当前终端')).toBeTruthy();
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(screen.queryByText('已开始写入当前终端')).toBeNull();
  });
});
