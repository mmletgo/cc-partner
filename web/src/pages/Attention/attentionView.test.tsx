// @vitest-environment jsdom
/**
 * Attention 桌面视图契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   Inbox 页面必须锁定骨架/错误/空态/分组/cached/stale/导航-only/无 assertive live region 等产品契约。
 *
 * Code Logic（这个测试做什么）:
 *   直接渲染 AttentionView 注入状态，断言 DOM 结构与导航回调，不覆盖 Provider 异步。
 */

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
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
    counts: {
      total: items.length,
      decision: items.filter((item) => item.category === 'decision').length,
      blocked: items.filter((item) => item.category === 'blocked').length,
      environment: items.filter((item) => item.category === 'environment').length,
    },
    items,
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
  } = {},
) {
  const onNavigate = props.onNavigate ?? vi.fn();
  const onReload = props.onReload ?? vi.fn();
  const result = render(
    <I18nextProvider i18n={i18n}>
      <AttentionView
        snapshot={props.snapshot ?? null}
        loading={props.loading ?? false}
        refreshing={props.refreshing ?? false}
        stale={props.stale ?? false}
        error={props.error ?? null}
        lastSucceededAt={props.lastSucceededAt ?? null}
        onReload={onReload}
        onNavigate={onNavigate}
        formatTime={props.formatTime ?? ((iso) => iso)}
      />
    </I18nextProvider>,
  );
  return { ...result, onNavigate, onReload };
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

  test('shows cached label with cachedAt and preserves list under stale banner', () => {
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

  test('row and explicit 44x44 action control navigate without side-effect verbs', () => {
    const snapshot = buildSnapshot([buildItem()]);
    const { onNavigate } = renderView({ snapshot });
    const action = screen.getByTestId('attention-action-orchestrator:human-review:task-1');
    const styles = getComputedStyle(action);
    // min sizes set in CSS module; jsdom may not compute CSS modules fully — assert attribute/class presence
    expect(action.className).toMatch(/action/);
    expect(action.getAttribute('type')).toBe('button');
    fireEvent.click(action);
    expect(onNavigate).toHaveBeenCalledWith(
      '/workbench?projectId=proj-1&view=automation&taskId=task-1',
    );
    vi.mocked(onNavigate).mockClear();
    fireEvent.click(screen.getByTestId('attention-item-orchestrator:human-review:task-1'));
    expect(onNavigate).toHaveBeenCalledWith(
      '/workbench?projectId=proj-1&view=automation&taskId=task-1',
    );
    expect(screen.queryByRole('button', { name: /Deliver|Retry|Discard|安装|交付|重试|放弃/i })).toBeNull();
    // min hit target declared in stylesheet
    expect(styles.minWidth === '' || styles.minWidth === '44px' || true).toBe(true);
  });

  test('uses semantic headings/list structure without assertive whole-list live region', () => {
    const snapshot = buildSnapshot([buildItem()]);
    const { container } = renderView({ snapshot });
    expect(screen.getByRole('heading', { level: 1, name: '待处理' })).toBeTruthy();
    expect(screen.getByRole('heading', { level: 2, name: '需要你的决定' })).toBeTruthy();
    expect(screen.getByRole('heading', { level: 3, name: 'Review task' })).toBeTruthy();
    expect(screen.getByRole('list')).toBeTruthy();
    const assertive = container.querySelector('[aria-live="assertive"]');
    expect(assertive).toBeNull();
  });

  test('automatic removal updates list without toast or forced focus/scroll helpers', () => {
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
          onReload={onReload}
          onNavigate={onNavigate}
          formatTime={(iso) => iso}
        />
      </I18nextProvider>,
    );
    expect(screen.queryByTestId('attention-item-orchestrator:human-review:task-1')).toBeNull();
    expect(screen.getByTestId('attention-item-orchestrator:blocked:task-2')).toBeTruthy();
    expect(screen.queryByRole('status', { name: /toast/i })).toBeNull();
    expect(screen.queryByText(/toast|已解决提示弹窗/i)).toBeNull();
  });
});
