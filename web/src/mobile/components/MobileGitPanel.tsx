import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import {
  createHttpOrchestratorClientRequestId,
  httpWorkbenchTransport,
  workbenchHttp,
} from '@/api/workbenchHttp';
import { StatusMessage } from '@/components/primitives';
import {
  getUnknownMutationClientOperationId,
  isMutationSucceeded,
  isMutationUnknown,
  isWorkbenchMutationUnknownError,
} from '@/lib/asyncState/mutationOutcome';
import type {
  MutationIntent,
  WorkbenchGitCommit,
  WorkbenchProject,
  WorkbenchWorktree,
} from '@/lib/types';
import {
  buildMergeRemoveAuthority,
  reconcileWorkbenchMutation,
} from '@/lib/workbenchMutationReconciliation';
import {
  isMobileGitActionResponseCurrent,
  isMobileGitMergeResponseCurrent,
  isMobileMutationActionLocked,
  pickMobileMutationOperationId,
  resolveMobileMutationPhase,
  shouldReloadMobileGitCommitsAfterAction,
  type MobileGitActionContext,
  type MobileGitPanelAction,
  type MobileMutationPhase,
} from '../mobilePanelState';
import { executeMobileGitCommit } from '../mobileGitCommit';
import type { MobileHookRepair } from '../mobileHookRepair';
import { getMobileWorktreeStatusKind } from '../mobileWorkbenchState';
import { MobileHookRepairCard } from './MobileHookRepairCard';
import styles from '../MobileWorkbench.module.css';

export interface MobileGitPanelProps {
  project: WorkbenchProject | null;
  worktree: WorkbenchWorktree | null;
  busy?: boolean;
  onWorktreeChange?: (worktree: WorkbenchWorktree) => void;
  onMergeWorktree: (worktree: WorkbenchWorktree) => Promise<boolean>;
  onRefreshWorktrees?: (options?: {
    skipFileContextConfirm?: boolean;
    expectedProjectId?: string;
  }) => Promise<void> | void;
  /** 钩子 AI 修复启动后聚焦新终端并切到 terminal 面板。 */
  onFocusRepairSession?: (sessionId: string) => Promise<void> | void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git 提交列表需要用用户本地时间展示 commit authoredAt，便于手机端快速判断最近活动。
 *
 * Code Logic（这个函数做什么）:
 *   将 ISO 字符串格式化为本地时间；非法日期回退原始字符串，避免空白显示。
 */
function formatCommitDate(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString();
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git 操作失败时移动端需要展示后端返回的可读错误，并兼容 unknown 抛出值。
 *
 * Code Logic（这个函数做什么）:
 *   优先返回 Error.message；其它值转字符串。
 */
function getErrorMessage(reason: unknown): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  return String(reason);
}

/**
 * MobileGitPanel（移动端 Git 面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端 Workbench 需要能查看当前 worktree Git 状态、最近提交，并执行提交、推送和合并到主工作区。
 *   mutation 在 timeout/network 下必须稳定 operation id + ledger 对账，禁止盲重放。
 *
 * Code Logic（这个组件做什么）:
 *   通过 HTTP envelope API 执行 commit/push；unknown 后查询 ledger、刷新权威 worktree 并用 pure matrix 对账；
 *   mutationPhase 按 project/worktree 重置；typed unknown 判定；StatusMessage 提供 same-id 重新核对。
 */
export function MobileGitPanel({
  project,
  worktree,
  busy = false,
  onWorktreeChange,
  onMergeWorktree,
  onRefreshWorktrees,
  onFocusRepairSession,
}: MobileGitPanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const [commits, setCommits] = useState<WorkbenchGitCommit[]>([]);
  const [loading, setLoading] = useState<boolean>(false);
  const [actionBusy, setActionBusy] = useState<'commit' | 'push' | 'merge' | null>(null);
  const [mutationPhase, setMutationPhase] = useState<MobileMutationPhase>('idle');
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [hookRepair, setHookRepair] = useState<MobileHookRepair | null>(null);
  const requestIdRef = useRef<number>(0);
  const currentContextRef = useRef<MobileGitActionContext | null>(null);
  const commitOperationIdRef = useRef<string | null>(null);
  const pushOperationIdRef = useRef<string | null>(null);
  const mergeOperationIdRef = useRef<string | null>(null);
  const unknownKindRef = useRef<'commit' | 'push' | 'merge' | null>(null);
  const statusLabel = useMemo(() => {
    if (!worktree) return t('workbench:mobile.gitPanel.noWorktree');
    const statusKind = getMobileWorktreeStatusKind(worktree);
    if (statusKind === 'conflict') {
      return t('workbench:worktrees.status.conflict', { count: worktree.status.conflicts });
    }
    if (statusKind === 'dirty') {
      return t('workbench:worktrees.status.dirty', { count: worktree.status.changed });
    }
    return t('workbench:worktrees.status.clean');
  }, [t, worktree]);

  // Business Logic: project/worktree 切换后旧 unknown 锁不得污染新上下文。
  /* eslint-disable react-hooks/set-state-in-effect -- context 切换时必须同步清空 phase/error/ids */
  useEffect(() => {
    currentContextRef.current = project
      ? { projectId: project.id, worktreeId: worktree?.id ?? null }
      : null;
    setMutationPhase('idle');
    setError(null);
    setSuccess(null);
    setHookRepair(null);
    setActionBusy(null);
    commitOperationIdRef.current = null;
    pushOperationIdRef.current = null;
    mergeOperationIdRef.current = null;
    unknownKindRef.current = null;
  // 仅 project.id / worktree.id 驱动上下文重置；project 对象引用变化不重置 phase。
    // eslint-disable-next-line react-hooks/exhaustive-deps -- intentional id-only scope
  }, [project?.id, worktree?.id]);
  /* eslint-enable react-hooks/set-state-in-effect */

  /**
   * Business Logic（为什么需要这个函数）:
   *   当前 worktree 切换后 Git 面板必须刷新到对应提交历史，避免显示上一个工作区的提交。
   *
   * Code Logic（这个函数做什么）:
   *   调用 git.listCommits(projectId, worktreeId, 30)，用递增 request id 丢弃旧响应。
   */
  const loadCommits = useCallback(async (): Promise<void> => {
    if (!project) {
      requestIdRef.current += 1;
      setCommits([]);
      return;
    }
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;
    setLoading(true);
    setError(null);

    try {
      const nextCommits = await httpWorkbenchTransport.git.listCommits(
        project.id,
        worktree?.id ?? null,
        30,
      );
      if (requestIdRef.current !== requestId) return;
      setCommits(nextCommits);
    } catch (reason) {
      if (requestIdRef.current !== requestId) return;
      setError(`${t('workbench:errors.gitHistory')}: ${getErrorMessage(reason)}`);
    } finally {
      if (requestIdRef.current === requestId) {
        setLoading(false);
      }
    }
  }, [project, t, worktree]);

  /* eslint-disable react-hooks/set-state-in-effect -- Git 面板在 project/worktree 变化时需要重新加载提交列表 */
  useEffect(() => {
    void loadCommits();
    return () => {
      requestIdRef.current += 1;
    };
  }, [loadCommits]);
  /* eslint-enable react-hooks/set-state-in-effect */

  /**
   * Business Logic（为什么需要这个函数）:
   *   Git 操作完成后父组件需要同步最新 worktree 状态；merge 后源 worktree 可能被删除，不能再用旧 id 刷新提交历史。
   *
   * Code Logic（这个函数做什么）:
   *   commit/push 后刷新 worktree 与 commits；merge 后让旧 commits 请求失效、清空当前提交，再只刷新 worktrees。
   */
  const refreshAfterAction = useCallback(async (
    action: MobileGitPanelAction,
    actionContext: MobileGitActionContext,
  ): Promise<void> => {
    if (!shouldReloadMobileGitCommitsAfterAction(action)) {
      requestIdRef.current += 1;
      setCommits([]);
      setError(null);
      setLoading(false);
      await onRefreshWorktrees?.({ expectedProjectId: actionContext.projectId });
      return;
    }
    await onRefreshWorktrees?.({ expectedProjectId: actionContext.projectId });
    if (!isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) return;
    await loadCommits();
  }, [loadCommits, onRefreshWorktrees]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   typed unknown 后禁止盲重放；ledger 终态优先，否则 authority 矩阵对账。
   *
   * Code Logic（这个函数做什么）:
   *   查询 getMutationOperation；刷新 worktrees；merge 填充 source + mainContainsSourceHead。
   */
  const reconcileUnknownMutation = useCallback(
    async (
      clientOperationId: string,
      actionContext: MobileGitActionContext,
    ): Promise<'confirmedSucceeded' | 'confirmedFailed' | 'unknown'> => {
      setMutationPhase('reconciling');
      const ledger = await workbenchHttp.git
        .getMutationOperation(clientOperationId)
        .catch(() => null);
      if (!isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) {
        return 'unknown';
      }

      await onRefreshWorktrees?.({ expectedProjectId: actionContext.projectId });
      if (!isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) {
        return 'unknown';
      }

      const intent: MutationIntent | null = ledger?.intent ?? null;
      if (!intent) {
        if (ledger?.state === 'succeeded') return 'confirmedSucceeded';
        if (ledger?.state === 'failed') return 'confirmedFailed';
        return 'unknown';
      }

      if (ledger?.state === 'succeeded' || ledger?.state === 'failed') {
        return reconcileWorkbenchMutation(intent, ledger, {});
      }

      if (intent.kind === 'merge' || intent.kind === 'collectMerge' || intent.kind === 'remove') {
        try {
          const latest = await httpWorkbenchTransport.worktrees.list(actionContext.projectId);
          if (!isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) {
            return 'unknown';
          }
          let mainCommitHashes: string[] | undefined;
          if (intent.kind === 'merge' || intent.kind === 'collectMerge') {
            const main = latest.find((item) => item.isMain) ?? null;
            if (main) {
              try {
                const mainCommits = await httpWorkbenchTransport.git.listCommits(
                  actionContext.projectId,
                  main.id,
                  100,
                );
                if (!isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) {
                  return 'unknown';
                }
                mainCommitHashes = mainCommits.map((commit) => commit.hash);
              } catch {
                mainCommitHashes = undefined;
              }
            }
          }
          const authority = buildMergeRemoveAuthority(intent, latest, { mainCommitHashes });
          return reconcileWorkbenchMutation(intent, ledger, authority);
        } catch {
          return 'unknown';
        }
      }

      return reconcileWorkbenchMutation(intent, ledger, {});
    },
    [onRefreshWorktrees],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   unknown 锁住后用户需要 same-id 重新核对，而不是盲发新 mutation。
   *
   * Code Logic（这个函数做什么）:
   *   按 unknownKind 取对应 operation id，再跑 reconcileUnknownMutation。
   */
  const handleRetryReconcile = useCallback(async (): Promise<void> => {
    if (busy || !project || !worktree || mutationPhase !== 'unknown') return;
    const kind = unknownKindRef.current;
    if (!kind) return;
    const clientOperationId =
      kind === 'commit'
        ? commitOperationIdRef.current
        : kind === 'push'
          ? pushOperationIdRef.current
          : mergeOperationIdRef.current;
    if (!clientOperationId) return;
    const actionContext = { projectId: project.id, worktreeId: worktree.id };
    setActionBusy(kind);
    setError(null);
    setSuccess(null);
    try {
      const confirmed = await reconcileUnknownMutation(clientOperationId, actionContext);
      if (!isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) return;
      const phase = resolveMobileMutationPhase('unknown', confirmed);
      if (phase === 'confirmedSucceeded') {
        if (kind === 'commit') commitOperationIdRef.current = null;
        if (kind === 'push') pushOperationIdRef.current = null;
        if (kind === 'merge') mergeOperationIdRef.current = null;
        unknownKindRef.current = null;
        setMutationPhase('idle');
        setError(null);
        await refreshAfterAction(kind === 'merge' ? 'merge' : kind, actionContext);
      } else if (phase === 'confirmedFailed') {
        if (kind === 'commit') commitOperationIdRef.current = null;
        if (kind === 'push') pushOperationIdRef.current = null;
        if (kind === 'merge') mergeOperationIdRef.current = null;
        unknownKindRef.current = null;
        setMutationPhase('idle');
        setError(t('workbench:errors.mutationFailed'));
      } else {
        setMutationPhase('unknown');
        setError(t('workbench:errors.mutationUnknown'));
      }
    } finally {
      if (isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) {
        setActionBusy(null);
      }
    }
  }, [busy, mutationPhase, project, reconcileUnknownMutation, refreshAfterAction, t, worktree]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端需要触发后端 Claude Code 生成提交信息并提交当前 worktree 改动。
   *
   * Code Logic（这个函数做什么）:
   *   稳定 clientOperationId + workbenchHttp.git.commit envelope；unknown 对账，不盲重放。
   */
  const handleCommit = useCallback(async (): Promise<void> => {
    if (busy || !project || !worktree) return;
    if (isMobileMutationActionLocked(mutationPhase) && mutationPhase !== 'unknown') return;
    if (mutationPhase === 'unknown' && unknownKindRef.current !== 'commit') return;
    const actionContext = { projectId: project.id, worktreeId: worktree.id };
    const clientOperationId = pickMobileMutationOperationId(
      mutationPhase,
      commitOperationIdRef.current,
      createHttpOrchestratorClientRequestId(),
    );
    commitOperationIdRef.current = clientOperationId;
    setActionBusy('commit');
    setMutationPhase('busy');
    setError(null);
    setSuccess(null);
    setHookRepair(null);
    try {
      const outcome = await executeMobileGitCommit({
        worktreeId: worktree.id,
        clientOperationId,
        reconcileOnly: mutationPhase === 'unknown',
        isCurrent: () =>
          isMobileGitActionResponseCurrent(actionContext, currentContextRef.current),
        git: workbenchHttp.git,
      });
      if (outcome.type === 'stale') return;
      if (outcome.type === 'succeeded') {
        commitOperationIdRef.current = null;
        unknownKindRef.current = null;
        setMutationPhase('idle');
        setSuccess(t('workbench:mobile.gitPanel.commitSucceeded'));
        onWorktreeChange?.(outcome.worktree);
        await refreshAfterAction('commit', actionContext);
        return;
      }
      if (outcome.type === 'succeededRefresh') {
        commitOperationIdRef.current = null;
        unknownKindRef.current = null;
        setMutationPhase('idle');
        setError(null);
        setSuccess(t('workbench:mobile.gitPanel.commitSucceeded'));
        await refreshAfterAction('commit', actionContext);
        return;
      }
      if (outcome.type === 'failedHook') {
        commitOperationIdRef.current = null;
        unknownKindRef.current = null;
        setMutationPhase('idle');
        setHookRepair({
          kind: 'commit',
          hookFailure: outcome.hookFailure,
          clientOperationId,
        });
        return;
      }
      if (outcome.type === 'failed') {
        commitOperationIdRef.current = null;
        unknownKindRef.current = null;
        setMutationPhase('idle');
        setError(t('workbench:errors.mutationFailed'));
        return;
      }
      unknownKindRef.current = 'commit';
      setMutationPhase('unknown');
      setError(t('workbench:errors.mutationUnknown'));
    } catch (reason) {
      if (!isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) return;
      setMutationPhase('idle');
      commitOperationIdRef.current = null;
      unknownKindRef.current = null;
      setError(`${t('workbench:errors.commitWorktree')}: ${getErrorMessage(reason)}`);
    } finally {
      if (isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) {
        setActionBusy(null);
      }
    }
  }, [
    busy,
    mutationPhase,
    onWorktreeChange,
    project,
    refreshAfterAction,
    t,
    worktree,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   failedHook 后用户点「让 AI 修复」时，在 owning device 启动可见 Claude agent。
   *
   * Code Logic（这个函数做什么）:
   *   调 workbenchHttp.git.repairHookFailure；成功后写入 terminalSessionId 并聚焦新终端。
   */
  const handleRepairHookFailure = useCallback(async (): Promise<void> => {
    if (busy || !project || !worktree || !hookRepair) return;
    const actionContext = { projectId: project.id, worktreeId: worktree.id };
    setActionBusy('commit');
    setError(null);
    setSuccess(null);
    try {
      const repair = await workbenchHttp.git.repairHookFailure(
        worktree.id,
        hookRepair.hookFailure,
      );
      if (!isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) return;
      setHookRepair({ ...hookRepair, terminalSessionId: repair.terminalSessionId });
      await onFocusRepairSession?.(repair.terminalSessionId);
    } catch (reason) {
      if (!isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) return;
      setError(`${t('workbench:errors.commitWorktree')}: ${getErrorMessage(reason)}`);
    } finally {
      if (isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) {
        setActionBusy(null);
      }
    }
  }, [busy, hookRepair, onFocusRepairSession, project, t, worktree]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户决定不修也不重试时，必须能清掉 stale failedHook 卡片。
   *
   * Code Logic（这个函数做什么）:
   *   本地清空 hookRepair，不发起 IPC。
   */
  const handleDismissHookFailure = useCallback((): void => {
    setHookRepair(null);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   AI 修复完成后用户手动重试 commit，必须 mint 新 operation id。
   *
   * Code Logic（这个函数做什么）:
   *   清空 hookRepair 后走 handleCommit。
   */
  const handleRetryAfterRepair = useCallback((): void => {
    setHookRepair(null);
    void handleCommit();
  }, [handleCommit]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   当前 worktree 有可推送分支时，手机端需要能把本地提交推送到后端判定的远端。
   *
   * Code Logic（这个函数做什么）:
   *   稳定 clientOperationId + push envelope；unknown 对账不盲重放。
   */
  const handlePush = useCallback(async (): Promise<void> => {
    if (busy || !project || !worktree) return;
    if (isMobileMutationActionLocked(mutationPhase) && mutationPhase !== 'unknown') return;
    if (mutationPhase === 'unknown' && unknownKindRef.current !== 'push') return;
    const actionContext = { projectId: project.id, worktreeId: worktree.id };
    const clientOperationId = pickMobileMutationOperationId(
      mutationPhase,
      pushOperationIdRef.current,
      createHttpOrchestratorClientRequestId(),
    );
    pushOperationIdRef.current = clientOperationId;
    setActionBusy('push');
    setMutationPhase('busy');
    setError(null);
    setSuccess(null);
    try {
      if (mutationPhase === 'unknown') {
        const confirmed = await reconcileUnknownMutation(clientOperationId, actionContext);
        if (!isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) return;
        const phase = resolveMobileMutationPhase('unknown', confirmed);
        if (phase === 'confirmedSucceeded') {
          pushOperationIdRef.current = null;
          unknownKindRef.current = null;
          setMutationPhase('idle');
          setError(null);
          await refreshAfterAction('push', actionContext);
        } else if (phase === 'confirmedFailed') {
          pushOperationIdRef.current = null;
          unknownKindRef.current = null;
          setMutationPhase('idle');
          setError(t('workbench:errors.mutationFailed'));
        } else {
          unknownKindRef.current = 'push';
          setMutationPhase('unknown');
          setError(t('workbench:errors.mutationUnknown'));
        }
        return;
      }

      const envelope = await workbenchHttp.git.push({
        worktreeId: worktree.id,
        clientOperationId,
      });
      if (!isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) return;

      if (isMutationSucceeded(envelope)) {
        pushOperationIdRef.current = null;
        unknownKindRef.current = null;
        setMutationPhase('idle');
        onWorktreeChange?.(envelope.value);
        await refreshAfterAction('push', actionContext);
        return;
      }

      if (isMutationUnknown(envelope)) {
        const confirmed = await reconcileUnknownMutation(
          envelope.clientOperationId,
          actionContext,
        );
        if (!isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) return;
        const phase = resolveMobileMutationPhase('unknown', confirmed);
        if (phase === 'confirmedSucceeded') {
          pushOperationIdRef.current = null;
          unknownKindRef.current = null;
          setMutationPhase('idle');
          setError(null);
          await refreshAfterAction('push', actionContext);
        } else if (phase === 'confirmedFailed') {
          pushOperationIdRef.current = null;
          unknownKindRef.current = null;
          setMutationPhase('idle');
          setError(t('workbench:errors.mutationFailed'));
        } else {
          unknownKindRef.current = 'push';
          setMutationPhase('unknown');
          setError(t('workbench:errors.mutationUnknown'));
        }
      }
    } catch (reason) {
      if (!isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) return;
      setMutationPhase('idle');
      pushOperationIdRef.current = null;
      unknownKindRef.current = null;
      setError(`${t('workbench:errors.pushWorktree')}: ${getErrorMessage(reason)}`);
    } finally {
      if (isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) {
        setActionBusy(null);
      }
    }
  }, [
    busy,
    mutationPhase,
    onWorktreeChange,
    project,
    reconcileUnknownMutation,
    refreshAfterAction,
    t,
    worktree,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   功能 worktree 合回主工作区，或主工作区 collect-merge 可收集分支。
   *
   * Code Logic（这个函数做什么）:
   *   非主或 canCollectMerge 时委托父级 dirty guard 与 envelope merge；typed unknown 进入 unknown 相位。
   */
  const handleMerge = useCallback(async (): Promise<void> => {
    if (busy || !project || !worktree) return;
    if (!(!worktree.isMain || worktree.canCollectMerge)) return;
    if (isMobileMutationActionLocked(mutationPhase) && mutationPhase !== 'unknown') return;
    if (mutationPhase === 'unknown' && unknownKindRef.current !== 'merge') return;
    const actionContext = { projectId: project.id, worktreeId: worktree.id };
    setActionBusy('merge');
    setMutationPhase('busy');
    setError(null);
    setSuccess(null);
    try {
      if (mutationPhase === 'unknown' && mergeOperationIdRef.current) {
        const confirmed = await reconcileUnknownMutation(
          mergeOperationIdRef.current,
          actionContext,
        );
        if (!isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) return;
        const phase = resolveMobileMutationPhase('unknown', confirmed);
        if (phase === 'confirmedSucceeded') {
          mergeOperationIdRef.current = null;
          unknownKindRef.current = null;
          setMutationPhase('idle');
          setError(null);
          requestIdRef.current += 1;
          setCommits([]);
          await onRefreshWorktrees?.({ expectedProjectId: actionContext.projectId });
        } else if (phase === 'confirmedFailed') {
          mergeOperationIdRef.current = null;
          unknownKindRef.current = null;
          setMutationPhase('idle');
          setError(t('workbench:errors.mutationFailed'));
        } else {
          unknownKindRef.current = 'merge';
          setMutationPhase('unknown');
          setError(t('workbench:errors.mutationUnknown'));
        }
        return;
      }

      const didMerge = await onMergeWorktree(worktree);
      if (!didMerge) {
        setMutationPhase('idle');
        return;
      }
      if (!isMobileGitMergeResponseCurrent(actionContext, currentContextRef.current)) return;
      requestIdRef.current += 1;
      setCommits([]);
      setError(null);
      setLoading(false);
      mergeOperationIdRef.current = null;
      unknownKindRef.current = null;
      setMutationPhase('idle');
    } catch (reason) {
      if (!isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) return;
      if (isWorkbenchMutationUnknownError(reason)) {
        const opId = getUnknownMutationClientOperationId(reason);
        mergeOperationIdRef.current = opId;
        unknownKindRef.current = 'merge';
        setMutationPhase('unknown');
        setError(t('workbench:errors.mutationUnknown'));
      } else {
        setMutationPhase('idle');
        mergeOperationIdRef.current = null;
        unknownKindRef.current = null;
        setError(`${t('workbench:errors.mergeWorktree')}: ${getErrorMessage(reason)}`);
      }
    } finally {
      if (isMobileGitActionResponseCurrent(actionContext, currentContextRef.current)) {
        setActionBusy(null);
      }
    }
  }, [
    busy,
    mutationPhase,
    onMergeWorktree,
    onRefreshWorktrees,
    project,
    reconcileUnknownMutation,
    t,
    worktree,
  ]);

  const actionDisabled =
    busy ||
    actionBusy !== null ||
    !worktree ||
    isMobileMutationActionLocked(mutationPhase);

  return (
    <section className={styles.panel} aria-labelledby="mobile-git-panel-title">
      <div className={styles.panelHeaderRow}>
        <div className={styles.panelHeader}>
          <h1 id="mobile-git-panel-title">{t('workbench:mobile.gitPanel.title')}</h1>
        </div>
        <button
          type="button"
          className={styles.secondaryButton}
          disabled={!project || loading}
          onClick={() => void loadCommits()}
        >
          {t('workbench:refreshGitHistory')}
        </button>
      </div>

      {!project ? <p className={styles.panelState}>{t('workbench:mobile.gitPanel.noProject')}</p> : null}
      {loading ? <p className={styles.panelState}>{t('workbench:gitHistoryLoading')}</p> : null}
      {mutationPhase === 'reconciling' ? (
        <StatusMessage tone="info" className={styles.panelState}>
          {t('workbench:mobile.gitPanel.reconciling')}
        </StatusMessage>
      ) : null}
      {success ? (
        <StatusMessage tone="success" className={styles.panelState}>
          {success}
        </StatusMessage>
      ) : null}
      {error ? (
        <StatusMessage
          tone="danger"
          className={styles.panelError}
          action={
            mutationPhase === 'unknown' ? (
              <button
                type="button"
                className={styles.secondaryButton}
                disabled={actionBusy !== null || busy}
                onClick={() => void handleRetryReconcile()}
              >
                {t('workbench:mobile.gitPanel.retryReconcile')}
              </button>
            ) : undefined
          }
        >
          <span>{t('workbench:mobile.projectPanel.error')}</span>
          <span>{error}</span>
        </StatusMessage>
      ) : null}
      {hookRepair ? (
        <MobileHookRepairCard
          hookRepair={hookRepair}
          busy={actionBusy === 'commit' || busy}
          onRepair={() => void handleRepairHookFailure()}
          onRetry={handleRetryAfterRepair}
          onDismiss={handleDismissHookFailure}
        />
      ) : null}

      <section className={styles.mobileStatusCard} aria-label={t('workbench:mobile.gitPanel.statusAriaLabel')}>
        <div className={styles.mobileStatusGrid}>
          <span>{t('workbench:mobile.gitPanel.status')}</span>
          <strong>{statusLabel}</strong>
          <span>{t('workbench:mobile.gitPanel.branch')}</span>
          <strong>{worktree?.status.branch ?? worktree?.branch ?? t('workbench:emptyValue')}</strong>
          <span>{t('workbench:mobile.gitPanel.aheadBehind')}</span>
          <strong>
            {t('workbench:mobile.gitPanel.aheadBehindValue', {
              ahead: worktree?.status.ahead ?? 0,
              behind: worktree?.status.behind ?? 0,
            })}
          </strong>
          <span>{t('workbench:mobile.gitPanel.canPush')}</span>
          <strong>
            {worktree?.status.canPush
              ? t('workbench:mobile.gitPanel.canPushAllowed')
              : t('workbench:mobile.gitPanel.canPushBlocked')}
          </strong>
        </div>
        <div className={styles.mobileToolbar}>
          <button
            type="button"
            className={styles.mobileTerminalPrimaryButton}
            disabled={actionDisabled}
            aria-busy={actionBusy === 'commit' || undefined}
            aria-label={t('workbench:worktrees.commit')}
            onClick={() => void handleCommit()}
          >
            {actionBusy === 'commit'
              ? t('workbench:mobile.gitPanel.committing')
              : t('workbench:worktrees.commit')}
          </button>
          <button
            type="button"
            className={styles.secondaryButton}
            disabled={actionDisabled || !worktree?.status.canPush}
            aria-busy={actionBusy === 'push' || undefined}
            aria-label={t('workbench:worktrees.push')}
            onClick={() => void handlePush()}
          >
            {actionBusy === 'push'
              ? t('workbench:mobile.gitPanel.pushing')
              : t('workbench:worktrees.push')}
          </button>
          <button
            type="button"
            className={styles.secondaryButton}
            disabled={actionDisabled || !(!worktree?.isMain || worktree.canCollectMerge)}
            aria-busy={actionBusy === 'merge' || undefined}
            aria-label={t('workbench:worktrees.merge')}
            onClick={() => void handleMerge()}
          >
            {actionBusy === 'merge'
              ? t('workbench:mobile.gitPanel.merging')
              : t('workbench:worktrees.merge')}
          </button>
        </div>
      </section>

      <div className={styles.mobileList} aria-label={t('workbench:mobile.gitPanel.commitsAriaLabel')}>
        {commits.length === 0 && project && !loading ? (
          <p className={styles.panelState}>{t('workbench:gitHistoryEmpty')}</p>
        ) : null}
        {commits.map((commit) => (
          <article key={commit.hash} className={styles.mobileListItem}>
            <span className={styles.mobileListTitleRow}>
              <strong className={styles.mobileListTitle}>{commit.summary}</strong>
              <span className={styles.mobileBadge}>{commit.shortHash}</span>
            </span>
            <span className={styles.mobileListMeta}>
              {t('workbench:mobile.gitPanel.commitMeta', {
                author: commit.authorName,
                date: formatCommitDate(commit.authoredAt),
              })}
            </span>
            {commit.refs.length > 0 ? (
              <span className={styles.mobileBadgeRow}>
                {commit.refs.map((ref) => (
                  <span key={ref.fullName} className={styles.mobileBadge}>
                    {ref.name}
                  </span>
                ))}
              </span>
            ) : null}
          </article>
        ))}
      </div>
    </section>
  );
}
