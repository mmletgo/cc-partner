// @vitest-environment jsdom
/**
 * Attention 桌面表格视图契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   Inbox 页面必须锁定骨架/错误/空态/8 列表格/已读灰显/分类已读/导航-only 等产品契约。
 *
 * Code Logic（这个测试做什么）:
 *   直接渲染 AttentionView 注入状态，断言 DOM 结构与回调，不覆盖 Provider 异步。
 */

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import { countAttentionItems } from '@/lib/attention';
import type { AttentionItem, AttentionSnapshot } from '@/lib/types';
import { AttentionView } from './Attention';

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要最小合法条目，避免每个用例重复样板。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的 AttentionItem。
 */
function buildItem(overrides: Partial<AttentionItem> = {}): AttentionItem {
  return {
    id: 'orchestrator:human-review:task-1',
    category: 'decision',
    sourceKind: 'orchestratorHumanReview',
    title: 'Review task',
    summary: 'Need human review',
    updatedAt: '2026-07-11T12:00:00.000Z',
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
 *   多个用例共享快照构造。
 *
 * Code Logic（这个函数做什么）:
 *   用 items 生成 counts 与 AttentionSnapshot。
 */
function buildSnapshot(items: AttentionItem[]): AttentionSnapshot {
  return {
    generatedAt: '2026-07-11T12:05:00.000Z',
    counts: countAttentionItems(items),
    items,
    myDeviceId: 'device-test',
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   契约测试需要统一挂载 i18n，避免 t key 原样泄漏导致误判。
 *
 * Code Logic（这个函数做什么）:
 *   用 I18nextProvider 渲染 AttentionView。
 */
function renderView(
  props: Partial<React.ComponentProps<typeof AttentionView>> & {
    onNavigate?: (url: string) => void;
    onReload?: () => void;
    onMarkRead?: (ids: string[]) => void;
    onMarkUnread?: (ids: string[]) => void;
    onMarkAllRead?: () => void;
    onMarkCategoryRead?: (category: AttentionItem['category']) => void;
  } = {},
) {
  const onNavigate = props.onNavigate ?? vi.fn();
  const onReload = props.onReload ?? vi.fn();
  const onMarkRead = props.onMarkRead ?? vi.fn();
  const onMarkUnread = props.onMarkUnread ?? vi.fn();
  const onMarkAllRead = props.onMarkAllRead ?? vi.fn();
  const onMarkCategoryRead = props.onMarkCategoryRead ?? vi.fn();
  const result = render(
    <I18nextProvider i18n={i18n}>
      <AttentionView
        snapshot={props.snapshot ?? null}
        loading={props.loading ?? false}
        refreshing={props.refreshing ?? false}
        stale={props.stale ?? false}
        error={props.error ?? null}
        lastSucceededAt={props.lastSucceededAt ?? null}
        pendingReadIds={props.pendingReadIds ?? new Set<string>()}
        markError={props.markError ?? null}
        onReload={onReload}
        onNavigate={onNavigate}
        onMarkRead={onMarkRead}
        onMarkUnread={onMarkUnread}
        onMarkAllRead={onMarkAllRead}
        onMarkCategoryRead={onMarkCategoryRead}
        formatTime={props.formatTime ?? ((iso) => iso)}
      />
    </I18nextProvider>,
  );
  return {
    ...result,
    onNavigate,
    onReload,
    onMarkRead,
    onMarkUnread,
    onMarkAllRead,
    onMarkCategoryRead,
  };
}

describe('AttentionView contracts', () => {
  test('shows skeleton without list and without badge content on first load', () => {
    renderView({ loading: true, snapshot: null, error: null });
    expect(screen.getByTestId('attention-skeleton')).toBeTruthy();
    expect(screen.queryByTestId('attention-groups')).toBeNull();
    expect(screen.queryByTestId('attention-empty')).toBeNull();
    expect(screen.getByText('待处理')).toBeTruthy();
  });

  test('shows first error with reload control and no empty celebration', () => {
    const { onReload } = renderView({
      loading: false,
      snapshot: null,
      error: new Error('boom'),
    });
    expect(screen.getByTestId('attention-error')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '重新加载' }));
    expect(onReload).toHaveBeenCalledTimes(1);
    expect(screen.queryByTestId('attention-empty')).toBeNull();
    expect(screen.queryByText(/庆祝|metrics|统计/i)).toBeNull();
  });

  test('renders empty state without celebration or metrics', () => {
    renderView({
      snapshot: buildSnapshot([]),
      loading: false,
      error: null,
    });
    expect(screen.getByTestId('attention-empty').textContent).toContain('当前没有阻塞工作的事项');
    expect(screen.queryByTestId('attention-groups')).toBeNull();
    expect(screen.queryByText(/庆祝|metrics|统计卡/i)).toBeNull();
  });

  test('hides empty groups and keeps category order', () => {
    const snapshot = buildSnapshot([
      buildItem({
        id: 'workbench:dependency:tmux',
        category: 'environment',
        sourceKind: 'workbenchDependency',
        title: 'tmux missing',
        target: { kind: 'settings', tab: 'dependencies' },
      }),
      buildItem({
        id: 'orchestrator:human-review:task-1',
        category: 'decision',
        sourceKind: 'orchestratorHumanReview',
      }),
    ]);
    renderView({ snapshot });
    const groups = screen.getByTestId('attention-groups');
    expect(within(groups).queryByTestId('attention-group-blocked')).toBeNull();
    expect(within(groups).getByTestId('attention-group-decision')).toBeTruthy();
    expect(within(groups).getByTestId('attention-group-environment')).toBeTruthy();
    const headings = within(groups)
      .getAllByRole('heading', { level: 2 })
      .map((node) => node.textContent);
    expect(headings).toEqual(['需要你的决定', '环境受阻']);
  });

  test('renders eight table headers and row cells', () => {
    renderView({ snapshot: buildSnapshot([buildItem()]) });
    const table = screen.getByRole('table');
    const headers = within(table)
      .getAllByRole('columnheader')
      .map((node) => node.textContent);
    expect(headers).toEqual(['项目', '设备', '来源', '分类', '时间', '标题', '摘要', '操作']);
    const row = screen.getByTestId('attention-item-orchestrator:human-review:task-1');
    expect(row.getAttribute('role')).toBe('row');
    expect(within(row).getByText('Demo')).toBeTruthy();
    expect(within(row).getByText('编排复核')).toBeTruthy();
  });

  test('shows cached label with cachedAt and preserves table under stale banner', () => {
    const item = buildItem({
      id: 'orchestrator:blocked:remote-1',
      category: 'blocked',
      sourceKind: 'orchestratorBlocked',
      freshness: 'cached',
      cachedAt: '2026-07-11T09:30:00.000Z',
      title: 'Remote blocked',
      target: { kind: 'orchestratorTask', projectId: 'proj-1', taskId: 'remote:dev:t1' },
    });
    const snapshot = buildSnapshot([item]);
    renderView({
      snapshot,
      stale: true,
      lastSucceededAt: '2026-07-11T09:31:00.000Z',
    });
    expect(screen.getByTestId('attention-stale-banner')).toBeTruthy();
    expect(
      screen.getByTestId('attention-cached-orchestrator:blocked:remote-1').textContent,
    ).toContain('2026-07-11T09:30:00.000Z');
    expect(screen.getByTestId('attention-item-orchestrator:blocked:remote-1')).toBeTruthy();
  });

  test('open action navigates and marks unread item read without side-effect verbs', () => {
    const snapshot = buildSnapshot([buildItem()]);
    const { onNavigate, onMarkRead } = renderView({ snapshot });
    const action = screen.getByTestId('attention-action-orchestrator:human-review:task-1');
    expect(action.tagName).toBe('BUTTON');
    fireEvent.click(action);
    expect(onMarkRead).toHaveBeenCalledWith(['orchestrator:human-review:task-1']);
    expect(onNavigate).toHaveBeenCalledWith(
      '/workbench?projectId=proj-1&view=automation&taskId=task-1',
    );
    expect(screen.queryByRole('button', { name: /Deliver|Retry|Discard|安装|交付|重试|放弃/i })).toBeNull();
  });

  test('row is not a single button; open and toggle-read are sibling controls', () => {
    const snapshot = buildSnapshot([buildItem()]);
    renderView({ snapshot });
    const row = screen.getByTestId('attention-item-orchestrator:human-review:task-1');
    expect(row.tagName).toBe('DIV');
    expect(row.getAttribute('role')).toBe('row');
    const buttons = within(row).getAllByRole('button');
    expect(buttons.length).toBe(2);
    expect(screen.getByTestId('attention-toggle-read-orchestrator:human-review:task-1')).toBeTruthy();
  });

  test('mark all read and category read are enabled only when unread exists', () => {
    const unread = buildSnapshot([buildItem()]);
    const { onMarkAllRead, onMarkCategoryRead, rerender, onNavigate, onReload } = renderView({
      snapshot: unread,
    });
    fireEvent.click(screen.getByRole('button', { name: '全部已读' }));
    expect(onMarkAllRead).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByTestId('attention-mark-category-decision'));
    expect(onMarkCategoryRead).toHaveBeenCalledWith('decision');

    const readItem = buildItem({ readAt: '2026-07-11T13:00:00.000Z' });
    rerender(
      <I18nextProvider i18n={i18n}>
        <AttentionView
          snapshot={buildSnapshot([readItem])}
          loading={false}
          refreshing={false}
          stale={false}
          error={null}
          lastSucceededAt={null}
          pendingReadIds={new Set()}
          markError={null}
          onReload={onReload}
          onNavigate={onNavigate}
          onMarkRead={vi.fn()}
          onMarkUnread={vi.fn()}
          onMarkAllRead={onMarkAllRead}
          onMarkCategoryRead={onMarkCategoryRead}
          formatTime={(iso) => iso}
        />
      </I18nextProvider>,
    );
    expect((screen.getByRole('button', { name: '全部已读' }) as HTMLButtonElement).disabled).toBe(
      true,
    );
    expect(
      (screen.getByTestId('attention-mark-category-decision') as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  test('read rows stay visible and can be marked unread', () => {
    const item = buildItem({ readAt: '2026-07-11T13:00:00.000Z' });
    const { onMarkUnread, onMarkRead } = renderView({ snapshot: buildSnapshot([item]) });
    const row = screen.getByTestId('attention-item-orchestrator:human-review:task-1');
    expect(row.getAttribute('data-read')).toBe('true');
    expect(row.className).toMatch(/itemRowRead/);
    fireEvent.click(screen.getByTestId('attention-toggle-read-orchestrator:human-review:task-1'));
    expect(onMarkUnread).toHaveBeenCalledWith(['orchestrator:human-review:task-1']);
    expect(onMarkRead).not.toHaveBeenCalled();
  });

  test('uses semantic headings/table structure without assertive whole-list live region', () => {
    const snapshot = buildSnapshot([buildItem()]);
    const { container } = renderView({ snapshot });
    expect(screen.getByRole('heading', { level: 1, name: '待处理' })).toBeTruthy();
    expect(screen.getByRole('heading', { level: 2, name: '需要你的决定' })).toBeTruthy();
    expect(screen.getByRole('table')).toBeTruthy();
    const assertive = container.querySelector('[aria-live="assertive"]');
    expect(assertive).toBeNull();
  });

  test('automatic removal updates table without toast or forced focus/scroll helpers', () => {
    const first = buildSnapshot([
      buildItem({ id: 'orchestrator:human-review:task-1' }),
      buildItem({
        id: 'orchestrator:blocked:task-2',
        category: 'blocked',
        sourceKind: 'orchestratorBlocked',
        title: 'Blocked',
        target: { kind: 'orchestratorTask', projectId: 'proj-1', taskId: 'task-2' },
      }),
    ]);
    const { rerender, onNavigate, onReload } = renderView({ snapshot: first });
    expect(screen.getByTestId('attention-item-orchestrator:human-review:task-1')).toBeTruthy();
    const next = buildSnapshot([
      buildItem({
        id: 'orchestrator:blocked:task-2',
        category: 'blocked',
        sourceKind: 'orchestratorBlocked',
        title: 'Blocked',
        target: { kind: 'orchestratorTask', projectId: 'proj-1', taskId: 'task-2' },
      }),
    ]);
    rerender(
      <I18nextProvider i18n={i18n}>
        <AttentionView
          snapshot={next}
          loading={false}
          refreshing={false}
          stale={false}
          error={null}
          lastSucceededAt={null}
          pendingReadIds={new Set()}
          markError={null}
          onReload={onReload}
          onNavigate={onNavigate}
          onMarkRead={vi.fn()}
          onMarkUnread={vi.fn()}
          onMarkAllRead={vi.fn()}
          onMarkCategoryRead={vi.fn()}
          formatTime={(iso) => iso}
        />
      </I18nextProvider>,
    );
    expect(screen.queryByTestId('attention-item-orchestrator:human-review:task-1')).toBeNull();
    expect(screen.getByTestId('attention-item-orchestrator:blocked:task-2')).toBeTruthy();
  });
});
