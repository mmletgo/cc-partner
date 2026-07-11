/**
 * Workbench controllers barrel —— 单点重新导出所有 Workbench 域 controller hooks 与公开类型。
 *
 * Business Logic（为什么需要这个文件）:
 *   Plan 2 Task 8 后 Workbench.tsx 同时引用多个 controller；barrel 让页面（与测试）只需 import 单一路径，
 *   并把 controller 之间的边界（哪些是 hook、哪些是 props 类型）集中可见。
 *
 * Code Logic（这个文件做什么）:
 *   只做 re-export，不引入任何实现或副作用；保留各 controller 子模块的导出形态。
 */
export { useWorkbenchAutomationController } from './useWorkbenchAutomationController';
export type {
  WorkbenchAutomationControllerParams,
  WorkbenchAutomationControllerResult,
} from './useWorkbenchAutomationController';

export { useWorkbenchFileController } from './useWorkbenchFileController';
export type {
  WorkbenchFileErrorKey,
  WorkbenchFileMessageKey,
  UseWorkbenchFileControllerParams,
  WorkbenchFileBridge,
  WorkbenchFileControllerResult,
} from './useWorkbenchFileController';

export { useWorkbenchProjectController } from './useWorkbenchProjectController';
export type {
  UseWorkbenchProjectControllerParams,
  WorkbenchProjectControllerResult,
} from './useWorkbenchProjectController';

export { useWorkbenchPromptOptimizerController, promptOptimizerPanelPosition } from './useWorkbenchPromptOptimizerController';
export type {
  PromptOptimizerPanelPosition,
  PromptOptimizerConfigLoadResult,
  PromptOptimizerStreamResult,
  PromptOptimizerStreamToTerminalOptions,
  UseWorkbenchPromptOptimizerControllerParams,
  WorkbenchPromptOptimizerControllerResult,
} from './useWorkbenchPromptOptimizerController';

export { useWorkbenchSessionSearchController } from './useWorkbenchSessionSearchController';
export type {
  UseWorkbenchSessionSearchControllerParams,
  WorkbenchSessionSearchControllerResult,
} from './useWorkbenchSessionSearchController';

export { useWorkbenchTerminalController } from './useWorkbenchTerminalController';
export type {
  WorkbenchPaneSplitDirection,
  TerminalCursorAnchor,
  TerminalSize,
  UseWorkbenchTerminalControllerParams,
  WorkbenchTerminalErrorKey,
  WorkbenchTerminalBridge,
  WorkbenchTerminalControllerResult,
} from './useWorkbenchTerminalController';

export { useWorkbenchWorktreeGitController } from './useWorkbenchWorktreeGitController';
export type {
  WorktreeBusyKind,
  WorkbenchWorktreeGitErrorKey,
  UseWorkbenchWorktreeGitControllerParams,
  WorkbenchWorktreeGitControllerResult,
} from './useWorkbenchWorktreeGitController';
