import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  createHttpOrchestratorClientRequestId,
  httpWorkbenchTransport,
  workbenchHttp,
} from '@/api/workbenchHttp';
import {
  getUnknownMutationClientOperationId,
  isMutationSucceeded,
  isMutationUnknown,
  isWorkbenchMutationUnknownError,
  WorkbenchMutationUnknownError,
} from '@/lib/asyncState/mutationOutcome';
import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import {
  DEFAULT_WORKTREE_BRANCH_PREFIX,
  composeWorktreeBranchName,
  type WorktreeBranchPrefix,
} from '@/lib/workbenchWorktreeBranches';
import {
  buildMergeRemoveAuthority,
  reconcileWorkbenchMutation,
} from '@/lib/workbenchMutationReconciliation';
import { createWorktreeWithTerminalWindow } from '@/pages/Workbench/workbenchWorktrees';
import {
  isMobileMutationActionLocked,
  pickMobileMutationOperationId,
  resolveMobileMutationPhase,
  runMobileWorktreeRemovalFlow,
  type MobileMutationPhase,
  type MobileWorktreeRemovalPlan,
} from '../mobilePanelState';

interface MobileWorktreeBarRefreshOptions {
  skipFileContextConfirm?: boolean;
  expectedProjectId?: string;
}

const DEFAULT_CREATED_SESSION_SIZE = { cols: 80, rows: 24 };

export interface UseMobileWorktreeBarControllerParams {
  project: WorkbenchProject | null;
  worktrees: WorkbenchWorktree[];
  activeWorktreeId: string | null;
  controlsBusy: boolean;
  confirmSwitchToWorktree: (worktree: WorkbenchWorktree) => boolean;
  confirmActiveWorktreeChange: (worktree: WorkbenchWorktree | null) => boolean;
  applyCreated: (
    nextWorktrees: WorkbenchWorktree[],
    nextActive: WorkbenchWorktree | null,
    session: WorkbenchSession | null,
  ) => void;
  applyRemoval: (plan: MobileWorktreeRemovalPlan) => void;
  beginWorktreeOperation: () => () => void;
  refreshWorktrees: (options?: MobileWorktreeBarRefreshOptions) => Promise<void> | void;
  refreshSessions: () => Promise<void> | void;
}

export interface UseMobileWorktreeBarControllerResult {
  createOpen: boolean;
  createPrefix: WorktreeBranchPrefix;
  createSuffix: string;
  creating: boolean;
  removing: boolean;
  pendingRemoval: WorkbenchWorktree | null;
  error: string | null;
  mutationPhase: MobileMutationPhase;
  openCreate: () => void;
  cancelCreate: () => void;
  setCreatePrefix: (prefix: WorktreeBranchPrefix) => void;
  setCreateSuffix: (suffix: string) => void;
  createWorktree: () => Promise<void>;
  requestRemove: (worktree: WorkbenchWorktree) => void;
  cancelRemove: () => void;
  confirmRemove: () => Promise<void>;
  retryReconcile: () => Promise<void>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   创建/删除失败时条上需要可读错误，并兼容非 Error 抛出值。
 *
 * Code Logic（这个函数做什么）:
 *   优先 Error.message，否则 String(reason)。
 */
function getErrorMessage(reason: unknown): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  return String(reason);
}

/**
 * useMobileWorktreeBarController（移动端 worktree 条创建/删除）
 *
 * Business Logic（为什么需要这个 hook）:
 *   终端面板上的 worktree 条要像桌面条一样新建并自动开窗口、移除非主 worktree；
 *   删除必须走 envelope + ledger，禁止盲重放。
 *
 * Code Logic（这个 hook 做什么）:
 *   持有创建表单与 pending remove；create 复用 createWorktreeWithTerminalWindow；
 *   remove 走 runMobileWorktreeRemovalFlow + workbenchHttp.git.remove。
 */
export function useMobileWorktreeBarController(
  params: UseMobileWorktreeBarControllerParams,
): UseMobileWorktreeBarControllerResult {
  const {
    project,
    worktrees,
    activeWorktreeId,
    controlsBusy,
    confirmSwitchToWorktree,
    confirmActiveWorktreeChange,
    applyCreated,
    applyRemoval,
    beginWorktreeOperation,
    refreshWorktrees,
    refreshSessions,
  } = params;
  const { t } = useTranslation(['workbench']);
  const [createOpen, setCreateOpen] = useState(false);
  const [createPrefix, setCreatePrefix] = useState<WorktreeBranchPrefix>(
    DEFAULT_WORKTREE_BRANCH_PREFIX,
  );
  const [createSuffix, setCreateSuffix] = useState('');
  const [creating, setCreating] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [pendingRemoval, setPendingRemoval] = useState<WorkbenchWorktree | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [mutationPhase, setMutationPhase] = useState<MobileMutationPhase>('idle');
  const removeOperationIdRef = useRef<string | null>(null);
  const unknownWorktreeIdRef = useRef<string | null>(null);
  const mutationSequenceRef = useRef(0);
  const projectIdRef = useRef<string | null>(project?.id ?? null);
  const worktreesRef = useRef(worktrees);
  const activeWorktreeIdRef = useRef(activeWorktreeId);

  useEffect(() => {
    projectIdRef.current = project?.id ?? null;
  }, [project?.id]);

  useEffect(() => {
    worktreesRef.current = worktrees;
  }, [worktrees]);

  useEffect(() => {
    activeWorktreeIdRef.current = activeWorktreeId;
  }, [activeWorktreeId]);

  /* eslint-disable react-hooks/set-state-in-effect -- 离开项目时必须清空条上的表单/对账态 */
  useEffect(() => {
    mutationSequenceRef.current += 1;
    setCreateOpen(false);
    setCreateSuffix('');
    setCreating(false);
    setRemoving(false);
    setPendingRemoval(null);
    setError(null);
    setMutationPhase('idle');
    removeOperationIdRef.current = null;
    unknownWorktreeIdRef.current = null;
  }, [project?.id]);
  /* eslint-enable react-hooks/set-state-in-effect */

  const openCreate = useCallback((): void => {
    if (controlsBusy || creating || removing || isMobileMutationActionLocked(mutationPhase)) {
      return;
    }
    setCreateOpen(true);
    setError(null);
  }, [controlsBusy, creating, mutationPhase, removing]);

  const cancelCreate = useCallback((): void => {
    if (creating) return;
    setCreateOpen(false);
    setCreateSuffix('');
  }, [creating]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   条上新建 worktree 后应像桌面一样自动开一个绑定窗口并切过去。
   *
   * Code Logic（这个函数做什么）:
   *   compose 分支名 → createWorktreeWithTerminalWindow；窗口失败保留 worktree。
   */
  const createWorktree = useCallback(async (): Promise<void> => {
    if (!project) return;
    const branchName = composeWorktreeBranchName(createPrefix, createSuffix);
    if (!branchName) return;
    if (creating || controlsBusy || isMobileMutationActionLocked(mutationPhase)) return;
    setCreating(true);
    setError(null);
    const endWorktreeOperation = beginWorktreeOperation();
    try {
      const result = await createWorktreeWithTerminalWindow({
        projectId: project.id,
        branchName,
        createWorktree: (projectId, name, baseBranch) =>
          httpWorkbenchTransport.worktrees.create(projectId, name, baseBranch),
        createSession: (projectId, initialSize, worktreeId) =>
          httpWorkbenchTransport.sessions.create(
            projectId,
            initialSize ?? DEFAULT_CREATED_SESSION_SIZE,
            worktreeId,
          ),
        initialSize: DEFAULT_CREATED_SESSION_SIZE,
      });
      if (projectIdRef.current !== project.id) return;
      const nextWorktrees = [
        ...worktreesRef.current.filter((item) => item.id !== result.worktree.id),
        result.worktree,
      ];
      const shouldSwitch = confirmSwitchToWorktree(result.worktree);
      const nextActive = shouldSwitch
        ? result.worktree
        : worktreesRef.current.find((item) => item.id === activeWorktreeIdRef.current)
          ?? result.worktree;
      applyCreated(nextWorktrees, nextActive, result.session);
      setCreateOpen(false);
      setCreateSuffix('');
      if (result.sessionError) {
        setError(
          `${t('workbench:errors.createSession')}: ${getErrorMessage(result.sessionError)}`,
        );
      }
      await refreshWorktrees({ expectedProjectId: project.id });
      await refreshSessions();
    } catch (reason) {
      if (projectIdRef.current !== project.id) return;
      setError(`${t('workbench:errors.createWorktree')}: ${getErrorMessage(reason)}`);
    } finally {
      endWorktreeOperation();
      if (projectIdRef.current === project.id) {
        setCreating(false);
      }
    }
  }, [
    applyCreated,
    beginWorktreeOperation,
    confirmSwitchToWorktree,
    controlsBusy,
    createPrefix,
    createSuffix,
    creating,
    mutationPhase,
    project,
    refreshSessions,
    refreshWorktrees,
    t,
  ]);

  const requestRemove = useCallback((worktree: WorkbenchWorktree): void => {
    if (worktree.isMain) return;
    if (controlsBusy || creating || removing || isMobileMutationActionLocked(mutationPhase)) {
      return;
    }
    setPendingRemoval(worktree);
  }, [controlsBusy, creating, mutationPhase, removing]);

  const cancelRemove = useCallback((): void => {
    if (removing) return;
    setPendingRemoval(null);
  }, [removing]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   unknown 后按 ledger 终态或 authority 对账，禁止 mint 新 id。
   *
   * Code Logic（这个函数做什么）:
   *   查 getMutationOperation；必要时 list + reconcileWorkbenchMutation。
   */
  const reconcileRemove = useCallback(
    async (
      clientOperationId: string,
      worktree: WorkbenchWorktree,
    ): Promise<'confirmedSucceeded' | 'confirmedFailed' | 'unknown'> => {
      const ledger = await workbenchHttp.git
        .getMutationOperation(clientOperationId)
        .catch(() => null);
      await refreshWorktrees({ expectedProjectId: worktree.projectId });
      const intent = ledger?.intent ?? null;
      if (ledger?.state === 'succeeded') return 'confirmedSucceeded';
      if (ledger?.state === 'failed') return 'confirmedFailed';
      if (!intent || intent.kind !== 'remove') return 'unknown';
      try {
        const latest = await httpWorkbenchTransport.worktrees.list(worktree.projectId);
        const authority = buildMergeRemoveAuthority(intent, latest, {});
        return reconcileWorkbenchMutation(intent, ledger, authority);
      } catch {
        return 'unknown';
      }
    },
    [refreshWorktrees],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   Dialog 确认后删除非主 worktree，成功才改列表/active。
   *
   * Code Logic（这个函数做什么）:
   *   runMobileWorktreeRemovalFlow + git.remove envelope；unknown 锁 same-id。
   */
  const confirmRemove = useCallback(async (): Promise<void> => {
    const worktree = pendingRemoval;
    if (!worktree || worktree.isMain) return;
    setPendingRemoval(null);
    setRemoving(true);
    setMutationPhase('busy');
    setError(null);
    const settledSequence = mutationSequenceRef.current + 1;
    mutationSequenceRef.current = settledSequence;
    const clientOperationId = pickMobileMutationOperationId(
      mutationPhase,
      removeOperationIdRef.current,
      createHttpOrchestratorClientRequestId(),
    );
    removeOperationIdRef.current = clientOperationId;
    const endWorktreeOperation = beginWorktreeOperation();
    try {
      const result = await runMobileWorktreeRemovalFlow({
        worktrees: worktreesRef.current,
        activeWorktreeId: activeWorktreeIdRef.current,
        removingWorktree: worktree,
        confirmActiveWorktreeChange,
        removeWorktree: async () => {
          const envelope = await workbenchHttp.git.remove({
            worktreeId: worktree.id,
            force: false,
            clientOperationId,
          });
          if (mutationSequenceRef.current !== settledSequence) return;
          if (isMutationSucceeded(envelope)) {
            removeOperationIdRef.current = null;
            unknownWorktreeIdRef.current = null;
            setMutationPhase('idle');
            return;
          }
          if (isMutationUnknown(envelope)) {
            const confirmed = await reconcileRemove(envelope.clientOperationId, worktree);
            if (mutationSequenceRef.current !== settledSequence) return;
            const phase = resolveMobileMutationPhase('unknown', confirmed);
            if (phase === 'confirmedSucceeded') {
              removeOperationIdRef.current = null;
              unknownWorktreeIdRef.current = null;
              setMutationPhase('idle');
              return;
            }
            if (phase === 'confirmedFailed') {
              removeOperationIdRef.current = null;
              unknownWorktreeIdRef.current = null;
              setMutationPhase('idle');
              throw new Error(t('workbench:errors.mutationFailed'));
            }
            unknownWorktreeIdRef.current = worktree.id;
            setMutationPhase('unknown');
            throw new WorkbenchMutationUnknownError(envelope.clientOperationId);
          }
        },
        applyRemoval: (plan) => {
          if (mutationSequenceRef.current !== settledSequence) return;
          applyRemoval(plan);
        },
      });
      if (mutationSequenceRef.current !== settledSequence) return;
      if (result === 'applied') {
        await refreshWorktrees({
          skipFileContextConfirm: activeWorktreeIdRef.current === worktree.id,
          expectedProjectId: worktree.projectId,
        });
        await refreshSessions();
      } else {
        setMutationPhase('idle');
        removeOperationIdRef.current = null;
        unknownWorktreeIdRef.current = null;
      }
    } catch (reason) {
      if (mutationSequenceRef.current !== settledSequence) return;
      if (isWorkbenchMutationUnknownError(reason) || unknownWorktreeIdRef.current === worktree.id) {
        removeOperationIdRef.current =
          getUnknownMutationClientOperationId(reason) ?? removeOperationIdRef.current;
        unknownWorktreeIdRef.current = worktree.id;
        setMutationPhase('unknown');
        setError(t('workbench:errors.mutationUnknown'));
      } else {
        setMutationPhase('idle');
        removeOperationIdRef.current = null;
        unknownWorktreeIdRef.current = null;
        setError(`${t('workbench:errors.removeWorktree')}: ${getErrorMessage(reason)}`);
      }
    } finally {
      endWorktreeOperation();
      if (mutationSequenceRef.current === settledSequence) {
        setRemoving(false);
      }
    }
  }, [
    applyRemoval,
    beginWorktreeOperation,
    confirmActiveWorktreeChange,
    mutationPhase,
    pendingRemoval,
    reconcileRemove,
    refreshSessions,
    refreshWorktrees,
    t,
  ]);

  const retryReconcile = useCallback(async (): Promise<void> => {
    if (mutationPhase !== 'unknown' || !unknownWorktreeIdRef.current || !removeOperationIdRef.current) {
      return;
    }
    const worktreeId = unknownWorktreeIdRef.current;
    const clientOperationId = removeOperationIdRef.current;
    const projectId = project?.id;
    if (!projectId) return;
    const target = worktrees.find((item) => item.id === worktreeId);
    const worktreeStub: WorkbenchWorktree =
      target
      ?? ({
        id: worktreeId,
        projectId,
      } as WorkbenchWorktree);
    setRemoving(true);
    setError(null);
    const settledSequence = mutationSequenceRef.current + 1;
    mutationSequenceRef.current = settledSequence;
    try {
      const confirmed = await reconcileRemove(clientOperationId, worktreeStub);
      if (mutationSequenceRef.current !== settledSequence) return;
      const phase = resolveMobileMutationPhase('unknown', confirmed);
      if (phase === 'confirmedSucceeded') {
        removeOperationIdRef.current = null;
        unknownWorktreeIdRef.current = null;
        setMutationPhase('idle');
        setError(null);
        await refreshWorktrees({ expectedProjectId: worktreeStub.projectId });
        await refreshSessions();
      } else if (phase === 'confirmedFailed') {
        removeOperationIdRef.current = null;
        unknownWorktreeIdRef.current = null;
        setMutationPhase('idle');
        setError(t('workbench:errors.mutationFailed'));
      } else {
        setMutationPhase('unknown');
        setError(t('workbench:errors.mutationUnknown'));
      }
    } finally {
      if (mutationSequenceRef.current === settledSequence) {
        setRemoving(false);
      }
    }
  }, [mutationPhase, project?.id, reconcileRemove, refreshSessions, refreshWorktrees, t, worktrees]);

  return {
    createOpen,
    createPrefix,
    createSuffix,
    creating,
    removing,
    pendingRemoval,
    error,
    mutationPhase,
    openCreate,
    cancelCreate,
    setCreatePrefix,
    setCreateSuffix,
    createWorktree,
    requestRemove,
    cancelRemove,
    confirmRemove,
    retryReconcile,
  };
}
