// @vitest-environment jsdom
/**
 * 切终端自动已读 hook 测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   看见等待输入的终端必须立刻降 Inbox 徽章，且不得与手动标未读打架。
 *
 * Code Logic（这个测试做什么）:
 *   用 AttentionProvider + 可控 markRead 覆盖：切到匹配终端、隐藏工作区不标、
 *   同一次聚焦不重标、离开后再回来会再标未读条目。
 */

import { afterEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';

import type { AttentionItem, AttentionSnapshot } from '@/lib/types';

import { AttentionProvider } from './useAttention';
import { useMarkNeedsInputAttentionOnSessionFocus } from './useMarkNeedsInputAttentionOnSessionFocus';

afterEach(() => {
  cleanup();
});

function buildItem(overrides: Partial<AttentionItem> = {}): AttentionItem {
  return {
    id: 'agent:needs-input:a1',
    category: 'decision',
    sourceKind: 'agentNeedsInput',
    title: 'Agent 等待输入',
    summary: '有 Agent 会话正在等待你的输入',
    updatedAt: '2026-08-16T10:00:00.000Z',
    freshness: 'live',
    cachedAt: null,
    project: { id: 'proj-1', name: 'Demo', kind: 'local' },
    device: null,
    target: {
      kind: 'agentSession',
      projectId: 'proj-1',
      terminalSessionId: 'term-1',
      agentSessionId: 'a1',
    },
    ...overrides,
  };
}

function buildSnapshot(items: AttentionItem[]): AttentionSnapshot {
  return {
    generatedAt: '2026-08-16T10:00:00.000Z',
    counts: {
      total: items.length,
      decision: items.length,
      blocked: 0,
      environment: 0,
      unreadTotal: items.length,
      unreadDecision: items.length,
      unreadBlocked: 0,
      unreadEnvironment: 0,
    },
    items,
    myDeviceId: 'device-test',
  };
}

function renderFocusHook(
  items: AttentionItem[],
  initial: { sessionId: string | null; enabled: boolean },
  markReadImpl?: (ids: string[]) => Promise<AttentionSnapshot>,
) {
  const snapshot = buildSnapshot(items);
  const markRead = vi.fn(async (ids: string[]) => {
    if (markReadImpl) return markReadImpl(ids);
    return snapshot;
  });
  const wrapper = ({ children }: { children: ReactNode }) => (
    <AttentionProvider
      loadSnapshot={async () => snapshot}
      mutations={{
        markRead,
        markUnread: vi.fn(async () => snapshot),
        markAllRead: vi.fn(async () => snapshot),
        markCategoryRead: vi.fn(async () => snapshot),
      }}
    >
      {children}
    </AttentionProvider>
  );
  return {
    markRead,
    ...renderHook(
      ({ sessionId, enabled }: { sessionId: string | null; enabled: boolean }) =>
        useMarkNeedsInputAttentionOnSessionFocus(sessionId, enabled),
      { wrapper, initialProps: initial },
    ),
  };
}

describe('useMarkNeedsInputAttentionOnSessionFocus', () => {
  test('marks matching unread needsInput when the terminal is visible', async () => {
    const { markRead } = renderFocusHook([buildItem()], { sessionId: 'term-1', enabled: true });
    await waitFor(() => {
      expect(markRead).toHaveBeenCalledWith(['agent:needs-input:a1']);
    });
  });

  test('does not mark when the terminal workspace is hidden', async () => {
    const { markRead } = renderFocusHook([buildItem()], { sessionId: 'term-1', enabled: false });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(markRead).not.toHaveBeenCalled();
  });

  test('does not rematch the same id while the session stays focused', async () => {
    const { markRead, rerender } = renderFocusHook([buildItem()], {
      sessionId: 'term-1',
      enabled: true,
    });
    await waitFor(() => {
      expect(markRead).toHaveBeenCalledTimes(1);
    });
    rerender({ sessionId: 'term-1', enabled: true });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(markRead).toHaveBeenCalledTimes(1);
  });

  test('marks again after leaving and returning to the waiting terminal', async () => {
    const { markRead, rerender } = renderFocusHook([buildItem()], {
      sessionId: 'term-1',
      enabled: true,
    });
    await waitFor(() => {
      expect(markRead).toHaveBeenCalledTimes(1);
    });
    rerender({ sessionId: 'term-1', enabled: false });
    rerender({ sessionId: 'term-1', enabled: true });
    await waitFor(() => {
      expect(markRead).toHaveBeenCalledTimes(2);
    });
  });
});
