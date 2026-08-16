// @vitest-environment jsdom
/**
 * MobileAttentionPanel 视图契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   紧凑分组列表、unsupported/stale/empty、无列表副作用动作是 Mobile Inbox 产品边界。
 *
 * Code Logic（这个测试做什么）:
 *   用 mock AttentionContext 渲染面板，断言分组文案、cached 文案、unsupported 与动作标签存在，
 *   并确认源码不含 Retry/Discard/Deliver 列表动作。
 */

import { readFileSync } from 'node:fs';
import type { ReactElement } from 'react';
import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import type { AttentionContextValue } from '@/hooks/attentionContext';
import i18n from '@/i18n';
import type { AttentionItem, AttentionSnapshot } from '@/lib/types';
import { MobileAttentionPanel } from './components/MobileAttentionPanel';

vi.mock('@/hooks/attentionContext', async () => {
  const actual = await vi.importActual<typeof import('@/hooks/attentionContext')>(
    '@/hooks/attentionContext',
  );
  return {
    ...actual,
    useAttention: () => mockAttentionValue,
  };
});

let mockAttentionValue: AttentionContextValue;

/**
 * Business Logic（为什么需要这个函数）:
 *   面板测试需要最小合法 AttentionItem。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的 AttentionItem。
 */
function buildItem(overrides: Partial<AttentionItem> = {}): AttentionItem {
  return {
    id: 'orchestrator:human-review:task-1',
    category: 'decision',
    sourceKind: 'orchestratorHumanReview',
    title: 'Review payment edge',
    summary: 'Need human review',
    updatedAt: '2026-07-11T10:00:00.000Z',
    freshness: 'live',
    cachedAt: null,
    project: { id: 'proj-1', name: 'Demo', kind: 'local' },
    device: null,
    target: { kind: 'orchestratorTask', projectId: 'proj-1', taskId: 'task-1' },
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Provider mock 需要完整 AttentionContextValue。
 *
 * Code Logic（这个函数做什么）:
 *   组装 snapshot/loading/stale/error/refresh 默认值。
 */
function buildContext(
  overrides: Partial<AttentionContextValue> = {},
): AttentionContextValue {
  return {
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
    ...overrides,
  };
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   组件测试必须挂 i18n，才能断言中文固定文案。
 *
 * Code Logic（这个函数做什么）:
 *   用 I18nextProvider 包裹 MobileAttentionPanel。
 */
function renderPanel(ui: ReactElement): ReturnType<typeof render> {
  return render(<I18nextProvider i18n={i18n}>{ui}</I18nextProvider>);
}

describe('MobileAttentionPanel', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   分组标题、原因、动作文案必须可见；列表不得提供 Retry/Discard/Deliver。
   *
   * Code Logic（这个测试做什么）:
   *   渲染含 decision 条目的快照，断言标题/摘要/动作；点击触发 onOpenItem。
   */
  test('renders compact grouped list and navigates on tap without side-effect actions', () => {
    const snapshot: AttentionSnapshot = {
      generatedAt: '2026-07-11T10:00:00.000Z',
      counts: {
        total: 1,
        decision: 1,
        blocked: 0,
        environment: 0,
        unreadTotal: 1,
        unreadDecision: 1,
        unreadBlocked: 0,
        unreadEnvironment: 0,
      },
      items: [buildItem()],
      myDeviceId: 'mobile-test',
    };
    const onOpenItem = vi.fn();
    mockAttentionValue = buildContext({
      snapshot,
      lastSucceededAt: '2026-07-11T10:00:00.000Z',
    });

    renderPanel(<MobileAttentionPanel onOpenItem={onOpenItem} />);

    expect(screen.getByRole('heading', { name: '待处理' })).toBeTruthy();
    expect(screen.getAllByText('需要你的决定').length).toBeGreaterThan(0);
    expect(screen.getByText('Review payment edge')).toBeTruthy();
    expect(screen.getByText('Need human review')).toBeTruthy();
    expect(screen.getByText('前往复核')).toBeTruthy();
    expect(screen.queryByText('重新发送')).toBeNull();
    expect(screen.queryByText('放弃')).toBeNull();

    fireEvent.click(screen.getByText('Review payment edge'));
    expect(onOpenItem).toHaveBeenCalledTimes(1);
    expect(onOpenItem.mock.calls[0]?.[0]?.id).toBe('orchestrator:human-review:task-1');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   legacy 后端无 attention.v1 时必须显示 unsupported，不能猜空列表。
   *
   * Code Logic（这个测试做什么）:
   *   mock AttentionHttpError(unsupported) 并断言 unsupported 文案。
   */
  test('shows unsupported state for missing attention.v1', async () => {
    const { AttentionHttpError } = await import('@/api/attentionHttp');
    mockAttentionValue = buildContext({
      error: new AttentionHttpError('no capability', 'unsupported', 'attention.v1'),
    });

    renderPanel(<MobileAttentionPanel onOpenItem={vi.fn()} />);

    expect(
      screen.getByText('当前后端不支持全局 Inbox（缺少 attention.v1）'),
    ).toBeTruthy();
    expect(screen.queryByText('当前没有阻塞工作的事项')).toBeNull();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   刷新失败但有快照时必须保留列表并显示 stale。
   *
   * Code Logic（这个测试做什么）:
   *   stale+snapshot 断言 stale banner 与条目仍在。
   */
  test('keeps items and shows stale banner when refresh fails with snapshot', () => {
    const snapshot: AttentionSnapshot = {
      generatedAt: '2026-07-11T10:00:00.000Z',
      counts: {
        total: 1,
        decision: 0,
        blocked: 1,
        environment: 0,
        unreadTotal: 1,
        unreadDecision: 0,
        unreadBlocked: 1,
        unreadEnvironment: 0,
      },
      myDeviceId: 'mobile-test',
      items: [
        buildItem({
          id: 'orchestrator:blocked:task-2',
          category: 'blocked',
          sourceKind: 'orchestratorBlocked',
          title: 'Blocked deploy',
          freshness: 'cached',
          cachedAt: '2026-07-11T09:00:00.000Z',
          target: { kind: 'orchestratorTask', projectId: 'proj-1', taskId: 'task-2' },
        }),
      ],
    };
    mockAttentionValue = buildContext({
      snapshot,
      stale: true,
      error: new Error('offline'),
      lastSucceededAt: '2026-07-11T09:30:00.000Z',
    });

    renderPanel(<MobileAttentionPanel onOpenItem={vi.fn()} />);

    expect(screen.getByText(/状态可能已过期/)).toBeTruthy();
    expect(screen.getByText('Blocked deploy')).toBeTruthy();
    expect(screen.getByText('远端缓存')).toBeTruthy();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   Mobile Inbox 列表侧禁止 Retry/Discard/Deliver 副作用动作。
   *
   * Code Logic（这个测试做什么）:
   *   静态扫描 MobileAttentionPanel 源码，确认不含这些动作文案/调用。
   */
  test('source never embeds list-side Retry Discard or Deliver actions', () => {
    const source = readFileSync(
      `${process.cwd()}/src/mobile/components/MobileAttentionPanel.tsx`,
      'utf8',
    );
    expect(source.includes('retry')).toBe(false);
    expect(source.includes('discard')).toBe(false);
    expect(source.includes('deliver')).toBe(false);
    expect(source.includes('Request Rework')).toBe(false);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   打开与标已读必须是兄弟按钮，禁止把已读控件嵌进打开按钮。
   *
   * Code Logic（这个测试做什么）:
   *   断言打开 button 内无嵌套 button，并存在独立 toggle。
   */
  test('each mobile attention row splits open and mark-read into sibling buttons', () => {
    const snapshot: AttentionSnapshot = {
      generatedAt: '2026-07-11T10:00:00.000Z',
      counts: {
        total: 1,
        decision: 1,
        blocked: 0,
        environment: 0,
        unreadTotal: 1,
        unreadDecision: 1,
        unreadBlocked: 0,
        unreadEnvironment: 0,
      },
      items: [buildItem()],
      myDeviceId: 'mobile-test',
    };
    mockAttentionValue = buildContext({ snapshot });
    renderPanel(<MobileAttentionPanel onOpenItem={vi.fn()} />);

    const open = screen.getByTestId('attention-action-orchestrator:human-review:task-1');
    expect(open.tagName).toBe('BUTTON');
    expect(open.querySelectorAll('button').length).toBe(0);
    expect(screen.getByTestId('attention-toggle-read-orchestrator:human-review:task-1')).toBeTruthy();
    const action = open.querySelector('[class*="actionLabel"]');
    expect(action?.tagName).toBe('SPAN');
  });
});
