/**
 * 桌面 / 移动端 Attention 投影 parity 测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   同一 snapshot 在桌面侧栏与移动导航必须显示相同 total badge 与分组顺序；
 *   计数与分组逻辑只能存在于共享 pure helper，不得在表面代码复制。
 *
 * Code Logic（这个测试做什么）:
 *   用一份 fixture 断言 formatAttentionBadgeCount 与 groupAttentionItems 结果，
 *   模拟桌面/移动两端消费同一 helper 的 parity。
 */

import { describe, expect, test } from 'vitest';

import {
  countUnreadAttentionItemsOnLocalDay,
  formatAttentionBadgeCount,
  groupAttentionItems,
  protectAttentionItemOrder,
} from './attention';
import type { AttentionItem, AttentionSnapshot } from './types';

/**
 * Business Logic（为什么需要这个函数）:
 *   parity 测试需要一份跨类别 fixture。
 *
 * Code Logic（这个函数做什么）:
 *   构造 decision/blocked/environment 混合 items 与 counts。
 */
function buildParitySnapshot(): AttentionSnapshot {
  const items: AttentionItem[] = [
    {
      id: 'workbench:dependency:tmux',
      category: 'environment',
      sourceKind: 'workbenchDependency',
      title: 'tmux',
      summary: 'missing',
      updatedAt: '2026-07-11T08:00:00.000Z',
      freshness: 'live',
      cachedAt: null,
      project: null,
      device: null,
      target: { kind: 'settings', tab: 'dependencies' },
    },
    {
      id: 'orchestrator:blocked:t2',
      category: 'blocked',
      sourceKind: 'orchestratorBlocked',
      title: 'blocked',
      summary: 'reason',
      updatedAt: '2026-07-11T11:00:00.000Z',
      freshness: 'live',
      cachedAt: null,
      project: { id: 'p1', name: 'Demo', kind: 'local' },
      device: null,
      target: { kind: 'orchestratorTask', projectId: 'p1', taskId: 't2' },
    },
    {
      id: 'orchestrator:human-review:t1',
      category: 'decision',
      sourceKind: 'orchestratorHumanReview',
      title: 'review',
      summary: 'need review',
      updatedAt: '2026-07-11T12:00:00.000Z',
      freshness: 'live',
      cachedAt: null,
      project: { id: 'p1', name: 'Demo', kind: 'local' },
      device: null,
      target: { kind: 'orchestratorTask', projectId: 'p1', taskId: 't1' },
    },
    {
      id: 'orchestrator:outbox-failed:ob1',
      category: 'blocked',
      sourceKind: 'remoteOutboxFailed',
      title: 'outbox',
      summary: 'failed',
      updatedAt: '2026-07-11T10:00:00.000Z',
      freshness: 'live',
      cachedAt: null,
      project: { id: 'remote:d1:x', name: 'Remote', kind: 'remote' },
      device: { id: 'd1', name: 'Mini' },
      target: { kind: 'remoteOutbox', projectId: 'remote:d1:x', outboxId: 'ob1' },
    },
  ];

  return {
    generatedAt: '2026-07-11T12:05:00.000Z',
    counts: {
      total: items.length,
      decision: 1,
      blocked: 2,
      environment: 1,
      unreadTotal: items.length,
      unreadDecision: 1,
      unreadBlocked: 2,
      unreadEnvironment: 1,
    },
    items,
    myDeviceId: 'd0',
  };
}

describe('attention desktop/mobile parity helpers', () => {
  test('same snapshot yields same badge total and group ordering', () => {
    const snapshot = buildParitySnapshot();

    // 桌面侧栏与移动 nav 都消费当天未读 + formatAttentionBadgeCount。
    const now = new Date(snapshot.items[0].updatedAt);
    const todayUnread = countUnreadAttentionItemsOnLocalDay(snapshot.items, now);
    const desktopBadge = formatAttentionBadgeCount(todayUnread);
    const mobileBadge = formatAttentionBadgeCount(todayUnread);
    expect(desktopBadge).toBe('4');
    expect(mobileBadge).toBe(desktopBadge);

    // 两端列表都只消费 groupAttentionItems，不在表面重算分类。
    const desktopGroups = groupAttentionItems(snapshot.items);
    const mobileGroups = groupAttentionItems(snapshot.items);
    expect(desktopGroups.map((group) => group.category)).toEqual([
      'decision',
      'blocked',
      'environment',
    ]);
    expect(mobileGroups.map((group) => group.category)).toEqual(
      desktopGroups.map((group) => group.category),
    );
    expect(desktopGroups.map((group) => group.items.map((item) => item.id))).toEqual(
      mobileGroups.map((group) => group.items.map((item) => item.id)),
    );

    const orderedIds = protectAttentionItemOrder(snapshot.items).map((item) => item.id);
    expect(orderedIds[0]).toBe('orchestrator:human-review:t1');
    expect(orderedIds).toContain('workbench:dependency:tmux');
  });
});
