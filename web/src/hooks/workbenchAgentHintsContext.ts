/**
 * 工作台 Agent hint Context。
 *
 * Business Logic（为什么需要这个模块）:
 *   侧栏 Rail 与 Workbench tab 必须读同一份全项目 hint；Provider 与 Context 分文件以满足 Fast Refresh。
 *
 * Code Logic（这个模块做什么）:
 *   定义 value 形状与 useWorkbenchAgentHintsContext。
 */

import { createContext, useContext } from 'react';
import type { UseWorkbenchAgentHintsResult } from './useWorkbenchAgentHints';

export type WorkbenchAgentHintsContextValue = UseWorkbenchAgentHintsResult;

export const WorkbenchAgentHintsContext = createContext<WorkbenchAgentHintsContextValue | null>(
  null,
);

/**
 * Business Logic（为什么需要这个函数）:
 *   Rail / Worktree / SessionTabs 需要读 hint selector。
 *
 * Code Logic（这个函数做什么）:
 *   缺 Provider 时抛出明确错误。
 */
export function useWorkbenchAgentHintsContext(): WorkbenchAgentHintsContextValue {
  const value = useContext(WorkbenchAgentHintsContext);
  if (!value) {
    throw new Error('useWorkbenchAgentHintsContext must be used inside WorkbenchAgentHintsProvider');
  }
  return value;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   孤立测试/未挂 Provider 的叶子不应崩溃，只是不显示数字。
 *
 * Code Logic（这个函数做什么）:
 *   返回 Context 或 null。
 */
export function useOptionalWorkbenchAgentHints(): WorkbenchAgentHintsContextValue | null {
  return useContext(WorkbenchAgentHintsContext);
}
