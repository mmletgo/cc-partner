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

import {
  getMobileAttentionNavigationPanel,
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
   *   tmux 依赖条目必须进入 Settings 依赖区域，不复制依赖安装组件。
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

  test('maps settings dependencies to settings panel', () => {
    const navigation = mapMobileAttentionTarget({
      kind: 'settings',
      tab: 'dependencies',
    });

    expect(navigation).toEqual({
      kind: 'settingsDependencies',
      panel: 'settings',
      tab: 'dependencies',
    });
    expect(getMobileAttentionNavigationPanel(navigation)).toBe('settings');
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
