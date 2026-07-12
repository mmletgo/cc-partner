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
  formatAttentionBadgeCount,
  getAttentionActionI18nKey,
  groupAttentionItems,
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
  });
});
