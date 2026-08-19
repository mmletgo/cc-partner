/**
 * 移动端 Attention target mapper 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   task/outbox/settings 三类语义跳转与缺失回退是 Mobile Inbox 导航契约，必须锁定。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 mapMobileAttentionTarget 三类映射与 resolveMobileAttentionMissingTargetPanel。
 */

import { describe, expect, test } from 'vitest';

import type { AttentionItem } from '@/lib/types';
import {
  filterMobileInboxAttentionItems,
  getMobileAttentionNavigationPanel,
  isMobileHiddenAttentionItem,
  mapMobileAttentionTarget,
  resolveMobileAttentionMissingTargetPanel,
} from './mobileAttentionTarget';

describe('mapMobileAttentionTarget', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   Human Review / Blocked 必须进入现有 Automation 面板并选中任务详情，不能开终端。
   *
   * Code Logic（这个测试做什么）:
   *   断言 orchestratorTask 映射为 automationTask + projectId/taskId。
   */
  test('maps orchestratorTask to automation task focus', () => {
    const navigation = mapMobileAttentionTarget({
      kind: 'orchestratorTask',
      projectId: 'proj-1',
      taskId: 'task-9',
    });

    expect(navigation).toEqual({
      kind: 'automationTask',
      projectId: 'proj-1',
      taskId: 'task-9',
      panel: 'automation',
    });
    expect(getMobileAttentionNavigationPanel(navigation)).toBe('automation');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   failed outbox 必须进入 Automation 并聚焦 outbox 行，列表内不执行 Retry/Discard。
   *
   * Code Logic（这个测试做什么）:
   *   断言 remoteOutbox 映射为 automationOutbox + outboxId。
   */
  test('maps remoteOutbox to automation outbox focus', () => {
    const navigation = mapMobileAttentionTarget({
      kind: 'remoteOutbox',
      projectId: 'proj-2',
      outboxId: 'outbox-3',
    });

    expect(navigation).toEqual({
      kind: 'automationOutbox',
      projectId: 'proj-2',
      outboxId: 'outbox-3',
      panel: 'automation',
    });
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   tmux 依赖条目无移动端权威界面，必须留在 Attention，不能打开依赖安装。
   *
   * Code Logic（这个测试做什么）:
   *   断言 settings/dependencies 映射为 settingsDependencies。
   */
  test('maps agentSession to terminal panel only', () => {
    const navigation = mapMobileAttentionTarget({
      kind: 'agentSession',
      projectId: 'proj-a',
      worktreeId: 'wt-1',
      terminalSessionId: 'term-9',
      agentSessionId: 'agent-1',
    });
    expect(navigation).toEqual({
      kind: 'terminalSession',
      projectId: 'proj-a',
      worktreeId: 'wt-1',
      sessionId: 'term-9',
      agentSessionId: 'agent-1',
      panel: 'terminal',
    });
    expect(getMobileAttentionNavigationPanel(navigation)).toBe('terminal');
  });

  test('maps settings dependencies back to attention (no mobile install surface)', () => {
    const navigation = mapMobileAttentionTarget({
      kind: 'settings',
      tab: 'dependencies',
    });

    expect(navigation).toEqual({
      kind: 'settingsDependencies',
      panel: 'attention',
      tab: 'dependencies',
    });
    expect(getMobileAttentionNavigationPanel(navigation)).toBe('attention');
  });

  test('maps agentHubAsset to attention panel (desktop-first Gate A)', () => {
    const navigation = mapMobileAttentionTarget({
      kind: 'agentHubAsset',
      assetId: 'asset-1',
      conflictId: 'conflict-1',
    });

    expect(navigation).toEqual({
      kind: 'agentHubAsset',
      assetId: 'asset-1',
      conflictId: 'conflict-1',
      panel: 'attention',
    });
    expect(getMobileAttentionNavigationPanel(navigation)).toBe('attention');
  });
});

describe('resolveMobileAttentionMissingTargetPanel', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   目标已解决时必须回到 Attention，避免空白详情或错误终端。
   *
   * Code Logic（这个测试做什么）:
   *   missing → attention；found → automation。
   */
  test('returns attention when focus target is missing', () => {
    expect(
      resolveMobileAttentionMissingTargetPanel({ status: 'missing', entity: 'task' }),
    ).toBe('attention');
    expect(
      resolveMobileAttentionMissingTargetPanel({ status: 'found', entity: 'outbox' }),
    ).toBe('automation');
  });
});

describe('filterMobileInboxAttentionItems', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   移动端不管理 tmux；Inbox 与 badge 必须剔除依赖条目，保留任务/终端类。
   *
   * Code Logic（这个测试做什么）:
   *   混入 workbenchDependency 与 settings target，断言只留下 orchestrator 条目。
   */
  test('hides tmux dependency items and keeps task items', () => {
    const task: AttentionItem = {
      id: 'orchestrator:human-review:task-1',
      category: 'decision',
      sourceKind: 'orchestratorHumanReview',
      title: 'Review',
      summary: 'Need review',
      updatedAt: '2026-07-11T10:00:00.000Z',
      freshness: 'live',
      cachedAt: null,
      project: { id: 'proj-1', name: 'Demo', kind: 'local' },
      device: null,
      target: { kind: 'orchestratorTask', projectId: 'proj-1', taskId: 'task-1' },
    };
    const tmux: AttentionItem = {
      id: 'workbench:dependency:tmux',
      category: 'environment',
      sourceKind: 'workbenchDependency',
      title: 'tmux missing',
      summary: 'Install tmux',
      updatedAt: '2026-07-11T10:00:00.000Z',
      freshness: 'live',
      cachedAt: null,
      project: null,
      device: null,
      target: { kind: 'settings', tab: 'dependencies' },
    };

    expect(isMobileHiddenAttentionItem(tmux)).toBe(true);
    expect(isMobileHiddenAttentionItem(task)).toBe(false);
    expect(filterMobileInboxAttentionItems([tmux, task]).map((item) => item.id)).toEqual([
      'orchestrator:human-review:task-1',
    ]);
  });
});
