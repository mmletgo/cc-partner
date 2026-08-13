/**
 * 工作台 Agent hint Provider。
 *
 * Business Logic（为什么需要这个组件）:
 *   侧栏常驻，必须在未进入 /workbench 时也握手全项目 waiting/completed。
 *
 * Code Logic（这个组件做什么）:
 *   调用 useWorkbenchAgentHints 并把结果放入 Context。
 */

import type { ReactNode } from 'react';
import { WorkbenchAgentHintsContext } from './workbenchAgentHintsContext';
import { useWorkbenchAgentHints } from './useWorkbenchAgentHints';

/**
 * Business Logic（为什么需要这个组件）:
 *   App 壳需要一份与 Rail 同生命周期的 hint 真值。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 Context.Provider。
 */
export function WorkbenchAgentHintsProvider({ children }: { children: ReactNode }) {
  const value = useWorkbenchAgentHints();
  return (
    <WorkbenchAgentHintsContext.Provider value={value}>
      {children}
    </WorkbenchAgentHintsContext.Provider>
  );
}
