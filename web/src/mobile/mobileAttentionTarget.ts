/**
 * 移动端 Attention 语义 target → panel/project/entity 映射。
 *
 * Business Logic（为什么需要这个模块）:
 *   后端只返回语义化 target，移动端必须把它转成现有 panel 导航与 Automation/Settings 聚焦参数，
 *   不能创建第二套详情或依赖设置组件。
 *
 * Code Logic（这个模块做什么）:
 *   纯函数把 AttentionTarget 映射为 MobileAttentionNavigation；并提供缺失目标回退与导航应用描述。
 */

import type { AttentionTarget } from '@/lib/types';
import type { MobileWorkbenchPanel } from './mobileWorkbenchState';

/**
 * 移动端 Inbox 导航意图。
 *
 * Business Logic（为什么需要这个类型）:
 *   MobileWorkbench 需要统一消费 task/outbox/settings 三类跳转，避免组件内 switch 漂移。
 *
 * Code Logic（字段说明）:
 *   automationTask/automationOutbox 带 projectId 与实体 id；settingsDependencies 只切 Settings 依赖区。
 */
export type MobileAttentionNavigation =
  | {
      kind: 'automationTask';
      projectId: string;
      taskId: string;
      panel: 'automation';
    }
  | {
      kind: 'automationOutbox';
      projectId: string;
      outboxId: string;
      panel: 'automation';
    }
  | {
      kind: 'settingsDependencies';
      panel: 'settings';
      tab: 'dependencies';
    }
  | {
      kind: 'terminalSession';
      projectId: string;
      worktreeId: string | null;
      sessionId: string;
      agentSessionId: string;
      panel: 'terminal';
    }
  | {
      kind: 'experiment';
      projectId: string;
      experimentId: string;
      panel: 'automation';
    };

/**
 * Business Logic（为什么需要这个函数）:
 *   点击 Inbox 条目后，移动端必须把语义 target 变成现有 panel 导航，而不是 URL。
 *
 * Code Logic（这个函数做什么）:
 *   orchestratorTask/outbox/settings 保持；agentSession → terminal；experiment → automation。
 */
export function mapMobileAttentionTarget(target: AttentionTarget): MobileAttentionNavigation {
  switch (target.kind) {
    case 'orchestratorTask':
      return {
        kind: 'automationTask',
        projectId: target.projectId,
        taskId: target.taskId,
        panel: 'automation',
      };
    case 'remoteOutbox':
      return {
        kind: 'automationOutbox',
        projectId: target.projectId,
        outboxId: target.outboxId,
        panel: 'automation',
      };
    case 'settings':
      return {
        kind: 'settingsDependencies',
        panel: 'settings',
        tab: 'dependencies',
      };
    case 'agentSession':
      return {
        kind: 'terminalSession',
        projectId: target.projectId,
        worktreeId: target.worktreeId ?? null,
        sessionId: target.terminalSessionId,
        agentSessionId: target.agentSessionId,
        panel: 'terminal',
      };
    case 'experiment':
      return {
        kind: 'experiment',
        projectId: target.projectId,
        experimentId: target.experimentId,
        panel: 'automation',
      };
    default: {
      const _exhaustive: never = target;
      return _exhaustive;
    }
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   父级应用导航时需要知道最终 panel，便于统一 setPanel 与关闭抽屉。
 *
 * Code Logic（这个函数做什么）:
 *   从 MobileAttentionNavigation 取出 panel 字段。
 */
export function getMobileAttentionNavigationPanel(
  navigation: MobileAttentionNavigation,
): MobileWorkbenchPanel {
  return navigation.panel;
}

/**
 * 目标聚焦结果。
 *
 * Business Logic（为什么需要这个类型）:
 *   Automation 面板聚焦 task/outbox 后要告诉父级成功或缺失，缺失时回退 Attention。
 *
 * Code Logic（字段说明）:
 *   found=true 表示实体存在；found=false 表示已解决或列表中找不到。
 */
export type MobileAttentionFocusResult =
  | { status: 'found'; entity: 'task' | 'outbox' }
  | { status: 'missing'; entity: 'task' | 'outbox' };

/**
 * Business Logic（为什么需要这个函数）:
 *   目标已解决或不存在时必须刷新 Inbox 并回到 Attention，不能进入空白详情。
 *
 * Code Logic（这个函数做什么）:
 *   缺失时返回 attention 面板；找到时保持 automation。
 */
export function resolveMobileAttentionMissingTargetPanel(
  result: MobileAttentionFocusResult,
): MobileWorkbenchPanel {
  return result.status === 'missing' ? 'attention' : 'automation';
}
