/**
 * Workbench workspace restore pure coordinator。
 *
 * Business Logic（为什么需要这个模块）:
 *   打开 Workbench 时先 preflight，再按顺序应用可安全恢复项；partial 只汇总一次。
 *   不渲染 UI；dirty editor 内容不被读取或覆盖。
 *
 * Code Logic（这个模块做什么）:
 *   capture previous selection → preflight → apply steps → 返回 summary；
 *   异常时恢复 previous selection。
 */

import type { InspectorTab, WorkspaceView } from './workspaceLayout';

/** preflight/apply 动作 outcome。 */
export type WorkspaceRestoreOutcome = 'select' | 'reuse' | 'safeAttach' | 'skip';

/** restore 计划状态。 */
export type RestorePlanStatus = 'complete' | 'partial' | 'offline' | 'empty';

/**
 * 单步动作。
 */
export interface WorkspaceRestoreAction {
  target: string;
  resourceId: string | null;
  outcome: WorkspaceRestoreOutcome;
  reason?: string | null;
}

/**
 * 后端 preflight 计划。
 */
export interface WorkspaceRestorePlan {
  restoreId: string;
  layoutId: string;
  layoutRevision: number;
  status: RestorePlanStatus;
  resolvedProjectId: string | null;
  resolvedWorktreeId: string | null;
  resolvedSessionId: string | null;
  workspaceView: WorkspaceView;
  inspectorTab: InspectorTab;
  browserTargetUrl: string | null;
  actions: WorkspaceRestoreAction[];
}

/**
 * 前端 selection 快照（可回滚）。
 */
export interface WorkspaceSelectionSnapshot {
  projectId: string | null;
  worktreeId: string | null;
  sessionId: string | null;
  workspaceView: WorkspaceView;
  inspectorTab: InspectorTab;
  browserTargetUrl: string | null;
  /** dirty editor 标记；restore 不得覆盖内容。 */
  dirtyEditor: boolean;
}

/**
 * apply 桥：只暴露现有 controller 能力。
 */
export interface WorkspaceRestoreBridge {
  selectProject: (projectId: string) => Promise<void> | void;
  selectWorktree: (worktreeId: string) => Promise<void> | void;
  focusSession: (sessionId: string) => Promise<void> | void;
  safeAttachSession: (sessionId: string) => Promise<void> | void;
  setWorkspaceView: (view: WorkspaceView) => void;
  setInspectorTab: (tab: InspectorTab) => void;
  restoreBrowserTarget: (url: string) => Promise<void> | void;
  applySelectionSnapshot: (snapshot: WorkspaceSelectionSnapshot) => Promise<void> | void;
}

/**
 * 一次恢复汇总。
 */
export interface WorkspaceRestoreSummary {
  restoreId: string;
  status: RestorePlanStatus;
  restoredCount: number;
  skippedCount: number;
  reasons: string[];
  /** 完全成功时 UI 应静默。 */
  silent: boolean;
  dirtyEditorPreserved: boolean;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   preflight 完成前不得改 UI；应用顺序固定。
 *
 * Code Logic（这个函数做什么）:
 *   等待 preflight → 按 project→worktree→session→view→inspector→browser 执行；
 *   异常恢复 previous；返回单次 summary。
 */
export async function applyWorkspaceRestorePlan(options: {
  previous: WorkspaceSelectionSnapshot;
  preflight: () => Promise<WorkspaceRestorePlan>;
  bridge: WorkspaceRestoreBridge;
  /** mobile v1：不自动应用 desktop layout。 */
  isMobile?: boolean;
}): Promise<WorkspaceRestoreSummary | null> {
  if (options.isMobile) {
    return null;
  }

  let plan: WorkspaceRestorePlan;
  try {
    plan = await options.preflight();
  } catch {
    return {
      restoreId: 'failed',
      status: 'empty',
      restoredCount: 0,
      skippedCount: 0,
      reasons: ['preflightFailed'],
      silent: true,
      dirtyEditorPreserved: options.previous.dirtyEditor,
    };
  }

  const reasons: string[] = [];
  let restored = 0;
  let skipped = 0;

  try {
    // 顺序：project → worktree → session → view → inspector → browser
    const ordered = sortActions(plan.actions);
    for (const action of ordered) {
      if (action.outcome === 'skip') {
        skipped += 1;
        if (action.reason) reasons.push(String(action.reason));
        continue;
      }
      switch (action.target) {
        case 'project':
          if (action.resourceId) {
            await options.bridge.selectProject(action.resourceId);
            restored += 1;
          }
          break;
        case 'worktree':
          if (action.resourceId) {
            await options.bridge.selectWorktree(action.resourceId);
            restored += 1;
          }
          break;
        case 'session':
          if (action.resourceId) {
            if (action.outcome === 'safeAttach') {
              await options.bridge.safeAttachSession(action.resourceId);
            } else {
              await options.bridge.focusSession(action.resourceId);
            }
            restored += 1;
          }
          break;
        case 'workspaceView':
          options.bridge.setWorkspaceView(plan.workspaceView);
          restored += 1;
          break;
        case 'inspectorTab':
          options.bridge.setInspectorTab(plan.inspectorTab);
          restored += 1;
          break;
        case 'browserTarget':
          if (action.resourceId && !options.previous.dirtyEditor) {
            await options.bridge.restoreBrowserTarget(action.resourceId);
            restored += 1;
          } else if (options.previous.dirtyEditor) {
            // dirty editor 不覆盖；browser 仍可恢复但文件内容不动
            if (action.resourceId) {
              await options.bridge.restoreBrowserTarget(action.resourceId);
              restored += 1;
            }
          }
          break;
        default:
          break;
      }
    }
  } catch {
    await options.bridge.applySelectionSnapshot(options.previous);
    return {
      restoreId: plan.restoreId,
      status: 'partial',
      restoredCount: restored,
      skippedCount: skipped + 1,
      reasons: [...reasons, 'applyException'],
      silent: false,
      dirtyEditorPreserved: options.previous.dirtyEditor,
    };
  }

  const status: RestorePlanStatus =
    skipped === 0 ? 'complete' : restored === 0 ? 'empty' : 'partial';

  return {
    restoreId: plan.restoreId,
    status,
    restoredCount: restored,
    skippedCount: skipped,
    reasons: unique(reasons),
    silent: status === 'complete',
    dirtyEditorPreserved: options.previous.dirtyEditor,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   UI notice 文案：已恢复 N 项，M 项已跳过。
 *
 * Code Logic（这个函数做什么）:
 *   中文摘要。
 */
export function formatRestoreNotice(summary: WorkspaceRestoreSummary): string {
  return `已恢复 ${summary.restoredCount} 项，${summary.skippedCount} 项已跳过`;
}

/** 与后端 has_skip 白名单同档：未请求，不算真正失败。 */
const NOT_REQUESTED_SKIP_REASONS = new Set([
  'browserSkippedForNonBrowserView',
  'browserNotRequested',
  'worktreeNotRequested',
  'sessionNotRequested',
]);

/** 良性 skip notice 展示后再自动关闭的时长。 */
export const TRANSIENT_RESTORE_NOTICE_MS = 4000;

/**
 * Business Logic（为什么需要这个函数）:
 *   首次打开强制 terminal 时跳过 leftover browser URL 是预期行为，
 *   提示应弹出后消失，不能钉在顶栏；真实失败必须留下。
 *
 * Code Logic（这个函数做什么）:
 *   仅当全部 reason 都是「未请求」白名单时视为 transient。
 */
export function isTransientRestoreNotice(summary: WorkspaceRestoreSummary): boolean {
  if (summary.silent || summary.status === 'complete' || summary.reasons.length === 0) {
    return false;
  }
  return summary.reasons.every((reason) => NOT_REQUESTED_SKIP_REASONS.has(reason));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   apply 整单失败时 notice 必须露出稳定 code，不能只写 applyFailed。
 *
 * Code Logic（这个函数做什么）:
 *   从 Error.code / message 映射有界 reason。
 */
export function classifyLayoutApplyError(error: unknown): string {
  const code = extractApplyErrorField(error, 'code');
  const message = extractApplyErrorField(error, 'message');
  const haystack = `${code} ${message}`;
  if (haystack.includes('workspace_layout_revision_changed')) {
    return 'layoutRevisionChanged';
  }
  if (haystack.includes('workspace_layout_not_found')) {
    return 'layoutMissing';
  }
  return 'applyFailed';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   invoke reject 可能是 Error 或 {code,message} 对象。
 *
 * Code Logic（这个函数做什么）:
 *   读指定字符串字段。
 */
function extractApplyErrorField(error: unknown, key: 'code' | 'message'): string {
  if (error instanceof Error) {
    if (key === 'message') return error.message;
    const record = error as Error & { code?: unknown };
    return typeof record.code === 'string' ? record.code : '';
  }
  if (error && typeof error === 'object') {
    const value = (error as Record<string, unknown>)[key];
    return typeof value === 'string' ? value : '';
  }
  return '';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   保证动作应用顺序与后端 preflight 一致。
 *
 * Code Logic（这个函数做什么）:
 *   按固定 target 权重排序，稳定排序。
 */
function sortActions(actions: WorkspaceRestoreAction[]): WorkspaceRestoreAction[] {
  const order: Record<string, number> = {
    project: 0,
    worktree: 1,
    session: 2,
    workspaceView: 3,
    inspectorTab: 4,
    browserTarget: 5,
  };
  return [...actions].sort(
    (a, b) => (order[a.target] ?? 99) - (order[b.target] ?? 99),
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   notice 展示去重 reason。
 *
 * Code Logic（这个函数做什么）:
 *   保序去重。
 */
function unique(values: string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const value of values) {
    if (seen.has(value)) continue;
    seen.add(value);
    out.push(value);
  }
  return out;
}
