/**
 * Attention 纯规则单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   badge、分组空态、排序保护、动作 key 与桌面 deep link 是跨端共享契约，回归必须锁定。
 *
 * Code Logic（这个测试做什么）:
 *   用 vitest 覆盖 badge 0/1..99/99+、分组省略、排序、sourceKind→key、三类 target URL。
 */

import { describe, expect, test } from 'vitest';

import {
  buildDesktopAttentionTargetUrl,
  collectUnreadAgentNeedsInputItemIds,
  formatAttentionBadgeCount,
  getAttentionActionI18nKey,
  groupAttentionItems,
  isIsoTimestampOnLocalDay,
  partitionAttentionItemsByLocalDay,
  planNeedsInputAttentionAutoRead,
  protectAttentionItemOrder,
} from './attention';
import type { AttentionItem } from './types';

/**
 * Business Logic（为什么需要这个函数）:
 *   测试只需构造字段完整的 AttentionItem，避免每个用例重复样板。
 *
 * Code Logic（这个函数做什么）:
 *   返回带默认值的 AttentionItem，允许 overrides 覆盖关键字段。
 */
function buildItem(overrides: Partial<AttentionItem> = {}): AttentionItem {
  return {
    id: 'orchestrator:human-review:task-1',
    category: 'decision',
    sourceKind: 'orchestratorHumanReview',
    title: 'Review task',
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

describe('formatAttentionBadgeCount', () => {
  test('returns null for zero and non-positive', () => {
    expect(formatAttentionBadgeCount(0)).toBeNull();
    expect(formatAttentionBadgeCount(-1)).toBeNull();
    expect(formatAttentionBadgeCount(Number.NaN)).toBeNull();
  });

  test('returns number string for 1..99', () => {
    expect(formatAttentionBadgeCount(1)).toBe('1');
    expect(formatAttentionBadgeCount(42)).toBe('42');
    expect(formatAttentionBadgeCount(99)).toBe('99');
  });

  test('returns 99+ for 100 and above', () => {
    expect(formatAttentionBadgeCount(100)).toBe('99+');
    expect(formatAttentionBadgeCount(1000)).toBe('99+');
  });
});

describe('groupAttentionItems', () => {
  test('groups by fixed category order and omits empty groups', () => {
    const items = [
      buildItem({
        id: 'workbench:dependency:tmux',
        category: 'environment',
        sourceKind: 'workbenchDependency',
        updatedAt: '2026-07-11T09:00:00.000Z',
        target: { kind: 'settings', tab: 'dependencies' },
      }),
      buildItem({
        id: 'orchestrator:blocked:task-2',
        category: 'blocked',
        sourceKind: 'orchestratorBlocked',
        updatedAt: '2026-07-11T11:00:00.000Z',
        target: { kind: 'orchestratorTask', projectId: 'proj-1', taskId: 'task-2' },
      }),
      buildItem({
        id: 'orchestrator:human-review:task-1',
        category: 'decision',
        sourceKind: 'orchestratorHumanReview',
        updatedAt: '2026-07-11T12:00:00.000Z',
      }),
    ];

    const groups = groupAttentionItems(items);
    expect(groups.map((g) => g.category)).toEqual(['decision', 'blocked', 'environment']);
    expect(groups.find((g) => g.category === 'decision')?.items).toHaveLength(1);
  });

  test('omits categories with no items', () => {
    const groups = groupAttentionItems([
      buildItem({
        id: 'orchestrator:blocked:only',
        category: 'blocked',
        sourceKind: 'orchestratorBlocked',
      }),
    ]);
    expect(groups.map((g) => g.category)).toEqual(['blocked']);
  });
});

describe('protectAttentionItemOrder', () => {
  test('orders by category then updatedAt desc then id asc', () => {
    const items = [
      buildItem({
        id: 'orchestrator:blocked:b',
        category: 'blocked',
        sourceKind: 'orchestratorBlocked',
        updatedAt: '2026-07-11T10:00:00.000Z',
      }),
      buildItem({
        id: 'orchestrator:human-review:z',
        category: 'decision',
        sourceKind: 'orchestratorHumanReview',
        updatedAt: '2026-07-11T09:00:00.000Z',
      }),
      buildItem({
        id: 'orchestrator:human-review:a',
        category: 'decision',
        sourceKind: 'orchestratorHumanReview',
        updatedAt: '2026-07-11T09:00:00.000Z',
      }),
      buildItem({
        id: 'orchestrator:human-review:newer',
        category: 'decision',
        sourceKind: 'orchestratorHumanReview',
        updatedAt: '2026-07-11T12:00:00.000Z',
      }),
    ];

    const ordered = protectAttentionItemOrder(items);
    expect(ordered.map((item) => item.id)).toEqual([
      'orchestrator:human-review:newer',
      'orchestrator:human-review:a',
      'orchestrator:human-review:z',
      'orchestrator:blocked:b',
    ]);
  });
});

describe('getAttentionActionI18nKey', () => {
  test('maps each sourceKind to a fixed i18n key', () => {
    expect(getAttentionActionI18nKey('orchestratorHumanReview')).toBe('attention:action.review');
    expect(getAttentionActionI18nKey('orchestratorBlocked')).toBe('attention:action.viewBlocked');
    expect(getAttentionActionI18nKey('remoteOutboxFailed')).toBe(
      'attention:action.viewFailed',
    );
    expect(getAttentionActionI18nKey('workbenchDependency')).toBe('attention:action.openSettings');
    expect(getAttentionActionI18nKey('agentHubConflict')).toBe('attention:action.openAgentHub');
    expect(getAttentionActionI18nKey('agentHubProjectionBlocked')).toBe(
      'attention:action.openAgentHub',
    );
  });
});

describe('buildDesktopAttentionTargetUrl', () => {
  test('maps semantic targets to the three approved desktop URLs', () => {
    expect(
      buildDesktopAttentionTargetUrl({
        kind: 'orchestratorTask',
        projectId: 'proj-1',
        taskId: 'task-9',
      }),
    ).toBe('/workbench?projectId=proj-1&view=automation&taskId=task-9');

    expect(
      buildDesktopAttentionTargetUrl({
        kind: 'remoteOutbox',
        projectId: 'remote:dev:proj',
        outboxId: 'outbox-3',
      }),
    ).toBe('/workbench?projectId=remote%3Adev%3Aproj&view=automation&outboxId=outbox-3');

    expect(
      buildDesktopAttentionTargetUrl({
        kind: 'settings',
        tab: 'dependencies',
      }),
    ).toBe('/settings?tab=dependencies');

    expect(
      buildDesktopAttentionTargetUrl({
        kind: 'agentHubAsset',
        assetId: 'asset-1',
        conflictId: 'c-9',
      }),
    ).toBe('/agent-hub?assetId=asset-1&conflictId=c-9');
  });
});

describe('partitionAttentionItemsByLocalDay', () => {
  const now = new Date(2026, 7, 16, 15, 0, 0);

  test('keeps same local calendar day and splits earlier items', () => {
    const today = buildItem({
      id: 'today',
      updatedAt: new Date(2026, 7, 16, 9, 0, 0).toISOString(),
    });
    const earlier = buildItem({
      id: 'earlier',
      updatedAt: new Date(2026, 7, 15, 18, 0, 0).toISOString(),
    });
    const partition = partitionAttentionItemsByLocalDay([today, earlier], now);
    expect(partition.today.map((item) => item.id)).toEqual(['today']);
    expect(partition.earlier.map((item) => item.id)).toEqual(['earlier']);
  });

  test('treats invalid timestamps as today so they are not hidden', () => {
    expect(isIsoTimestampOnLocalDay('not-a-date', now)).toBe(true);
    const invalid = buildItem({ id: 'invalid', updatedAt: 'not-a-date' });
    const partition = partitionAttentionItemsByLocalDay([invalid], now);
    expect(partition.today).toHaveLength(1);
    expect(partition.earlier).toHaveLength(0);
  });
});

describe('collectUnreadAgentNeedsInputItemIds', () => {
  test('keeps only unread needsInput items for the focused terminal', () => {
    const items = [
      buildItem({
        id: 'agent:needs-input:a1',
        sourceKind: 'agentNeedsInput',
        target: {
          kind: 'agentSession',
          projectId: 'proj-1',
          terminalSessionId: 'term-1',
          agentSessionId: 'a1',
        },
      }),
      buildItem({
        id: 'agent:needs-input:a2',
        sourceKind: 'agentNeedsInput',
        target: {
          kind: 'agentSession',
          projectId: 'proj-1',
          terminalSessionId: 'term-2',
          agentSessionId: 'a2',
        },
      }),
      buildItem({
        id: 'agent:failed:a3',
        category: 'blocked',
        sourceKind: 'agentFailed',
        target: {
          kind: 'agentSession',
          projectId: 'proj-1',
          terminalSessionId: 'term-1',
          agentSessionId: 'a3',
        },
      }),
      buildItem({
        id: 'agent:needs-input:read',
        sourceKind: 'agentNeedsInput',
        readAt: '2026-08-16T10:00:00.000Z',
        target: {
          kind: 'agentSession',
          projectId: 'proj-1',
          terminalSessionId: 'term-1',
          agentSessionId: 'read',
        },
      }),
    ];

    expect(collectUnreadAgentNeedsInputItemIds(items, 'term-1')).toEqual([
      'agent:needs-input:a1',
    ]);
    expect(collectUnreadAgentNeedsInputItemIds(items, '')).toEqual([]);
  });
});

describe('planNeedsInputAttentionAutoRead', () => {
  test('returns nothing when hidden, missing session, or already attempted', () => {
    const items = [
      buildItem({
        id: 'agent:needs-input:a1',
        sourceKind: 'agentNeedsInput',
        target: {
          kind: 'agentSession',
          projectId: 'proj-1',
          terminalSessionId: 'term-1',
          agentSessionId: 'a1',
        },
      }),
    ];

    expect(planNeedsInputAttentionAutoRead(items, 'term-1', false, new Set())).toEqual([]);
    expect(planNeedsInputAttentionAutoRead(items, null, true, new Set())).toEqual([]);
    expect(planNeedsInputAttentionAutoRead(undefined, 'term-1', true, new Set())).toEqual([]);
    expect(
      planNeedsInputAttentionAutoRead(items, 'term-1', true, new Set(['agent:needs-input:a1'])),
    ).toEqual([]);
    expect(planNeedsInputAttentionAutoRead(items, 'term-1', true, new Set())).toEqual([
      'agent:needs-input:a1',
    ]);
  });
});
