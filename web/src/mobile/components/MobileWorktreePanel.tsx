import { useCallback, useEffect, useRef, useState } from 'react';
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
  WorkbenchMutationUnknownError,
} from '@/lib/asyncState/mutationOutcome';
import type {
  MutationIntent,
  WorkbenchProject,
  WorkbenchWorktree,
} from '@/lib/types';
import {
  DEFAULT_WORKTREE_BRANCH_PREFIX,
  WORKTREE_BRANCH_PREFIXES,
  composeWorktreeBranchName,
  type WorktreeBranchPrefix,
} from '@/lib/workbenchWorktreeBranches';
import {
  buildMergeRemoveAuthority,
  reconcileWorkbenchMutation,
} from '@/lib/workbenchMutationReconciliation';
import {
  isMobileMutationActionLocked,
  pickMobileMutationOperationId,
  resolveMobileMutationPhase,
  runMobileWorktreeRemovalFlow,
  type MobileMutationPhase,
} from '../mobilePanelState';
import {
  canRunMobileWorktreeDestructiveAction,
  getMobileWorktreeStatusKind,
} from '../mobileWorkbenchState';
import styles from '../MobileWorkbench.module.css';

export interface MobileWorktreePanelProps {
  project: WorkbenchProject | null;
  worktrees: WorkbenchWorktree[];
  activeWorktreeId: string | null;
  busy?: boolean;
  onSelect: (worktree: WorkbenchWorktree) => boolean | void;
  onWorktreesChange?: (worktrees: WorkbenchWorktree[]) => void;
  onConfirmActiveWorktreeChange?: (worktree: WorkbenchWorktree | null) => boolean;
  onActiveWorktreeChange?: (worktree: WorkbenchWorktree | null) => void;
  onRefreshWorktrees?: (options?: {
    skipFileContextConfirm?: boolean;
    expectedProjectId?: string;
  }) => Promise<void> | void;
  onMergeWorktree?: (worktree: WorkbenchWorktree) => Promise<boolean> | boolean;
  onBeginWorktreeOperation?: () => () => void;
  onIsWorktreeActive?: (worktree: WorkbenchWorktree) => boolean;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 worktree 操作失败时需要展示可读错误，并兼容非 Error 抛出值。
 *
 * Code Logic（这个函数做什么）:
 *   优先返回 Error.message；其它 unknown 值转字符串。
 */
function getErrorMessage(reason: unknown): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  return String(reason);
}

/**
 * MobileWorktreePanel（移动端 worktree 列表面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   移动端用户需要完整管理项目 worktree，驱动终端、文件和 Git 面板使用同一个工作区上下文。
 *   remove 在 timeout/network 下必须稳定 operation id + ledger 对账。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 worktree 列表与创建/merge/remove；remove 走 envelope + 对账，禁止盲重放。
 */
export function MobileWorktreePanel({
  project,
  worktrees,
  activeWorktreeId,
  busy = false,
  onSelect,
  onWorktreesChange,
  onConfirmActiveWorktreeChange,
  onActiveWorktreeChange,
  onRefreshWorktrees,
  onMergeWorktree,
  onBeginWorktreeOperation,
  onIsWorktreeActive,
}: MobileWorktreePanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const [branchPrefix, setBranchPrefix] = useState<WorktreeBranchPrefix>(
    DEFAULT_WORKTREE_BRANCH_PREFIX,
  );
  const [branchSuffix, setBranchSuffix] = useState<string>('');
  const [actionBusy, setActionBusy] = useState<'create' | string | null>(null);
  const [mutationPhase, setMutationPhase] = useState<MobileMutationPhase>('idle');
  const [error, setError] = useState<string | null>(null);
  const removeOperationIdRef = useRef<string | null>(null);
  const mergeOperationIdRef = useRef<string | null>(null);
  const unknownKindRef = useRef<'remove' | 'merge' | null>(null);
  const unknownWorktreeIdRef = useRef<string | null>(null);
  const composedBranchName = composeWorktreeBranchName(branchPrefix, branchSuffix);
  const isEmpty = worktrees.length === 0;
  const isActionDisabled =
    busy || actionBusy !== null || isMobileMutationActionLocked(mutationPhase);

  // Business Logic: project/worktree 列表上下文变化后，旧 unknown 锁不得污染新上下文。
  /* eslint-disable react-hooks/set-state-in-effect -- context 切换时必须同步清空 phase/error/ids */
  useEffect(() => {
    setMutationPhase('idle');
    setError(null);
    setActionBusy(null);
    removeOperationIdRef.current = null;
    mergeOperationIdRef.current = null;
    unknownKindRef.current = null;
    unknownWorktreeIdRef.current = null;
  }, [project?.id, activeWorktreeId]);
  /* eslint-enable react-hooks/set-state-in-effect */

  /**
   * Business Logic（为什么需要这个函数）:
   *   创建 worktree 后，父组件需要同步列表，并在父级允许时同步 active worktree。
   *
   * Code Logic（这个函数做什么）:
   *   调用父级列表回调；active 非空时复用既有 onSelect，空 active 直接同步为空状态。
   */
  const applyWorktrees = useCallback(
    (nextWorktrees: WorkbenchWorktree[], nextActive: WorkbenchWorktree | null): boolean => {
      onWorktreesChange?.(nextWorktrees);
      if (nextActive) {
        return onSelect(nextActive) !== false;
      }
      onActiveWorktreeChange?.(null);
      return true;
    },
    [onActiveWorktreeChange, onSelect, onWorktreesChange],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端需要能从当前项目创建一个功能 worktree，供后续终端、文件和 Git 面板共同使用。
   *
   * Code Logic（这个函数做什么）:
   *   组合表单前缀与后缀，调用 worktrees.create(projectId, composedBranchName, null)，成功后追加到列表并设为 active。
   */
  const handleCreateWorktree = useCallback(async (): Promise<void> => {
    if (!project || !composedBranchName) return;
    setActionBusy('create');
    setError(null);
    const endWorktreeOperation = onBeginWorktreeOperation?.();
    try {
      const created = await httpWorkbenchTransport.worktrees.create(
        project.id,
        composedBranchName,
        null,
      );
      const nextWorktrees = [...worktrees.filter((item) => item.id !== created.id), created];
      endWorktreeOperation?.();
      const didApplyActive = applyWorktrees(nextWorktrees, created);
      setBranchSuffix('');
      if (didApplyActive) {
        await onRefreshWorktrees?.({ expectedProjectId: project.id });
      }
    } catch (reason) {
      setError(`${t('workbench:errors.createWorktree')}: ${getErrorMessage(reason)}`);
    } finally {
      endWorktreeOperation?.();
      setActionBusy(null);
    }
  }, [
    applyWorktrees,
    composedBranchName,
    onBeginWorktreeOperation,
    onRefreshWorktrees,
    project,
    t,
    worktrees,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   功能 worktree 完成后，用户需要能在完整 Worktrees 面板里直接合并回主工作区。
   *
   * Code Logic（这个函数做什么）:
   *   非主且非 busy 时调用父级 merge flow；父级负责 envelope 与成功后的列表刷新。
   */
  /**
   * Business Logic（为什么需要这个函数）:
   *   remove/merge unknown 后按 ledger 终态或 authority 矩阵对账，禁止盲重放。
   *
   * Code Logic（这个函数做什么）:
   *   查询 getMutationOperation；刷新列表；merge 填充 source + mainContainsSourceHead。
   */
  const reconcileUnknownMutation = useCallback(
    async (
      clientOperationId: string,
      worktree: WorkbenchWorktree,
      expectedKind: 'remove' | 'merge',
    ): Promise<'confirmedSucceeded' | 'confirmedFailed' | 'unknown'> => {
      setMutationPhase('reconciling');
      const ledger = await workbenchHttp.git
        .getMutationOperation(clientOperationId)
        .catch(() => null);

      await onRefreshWorktrees?.({
        expectedProjectId: worktree.projectId,
      });

      const intent: MutationIntent | null = ledger?.intent ?? null;
      if (!intent || intent.kind !== expectedKind) {
        if (ledger?.state === 'succeeded') return 'confirmedSucceeded';
        if (ledger?.state === 'failed') return 'confirmedFailed';
        return 'unknown';
      }

      if (ledger?.state === 'succeeded' || ledger?.state === 'failed') {
        return reconcileWorkbenchMutation(intent, ledger, {});
      }

      try {
        const latest = await httpWorkbenchTransport.worktrees.list(worktree.projectId);
        let mainCommitHashes: string[] | undefined;
        if (intent.kind === 'merge') {
          const main = latest.find((item) => item.isMain) ?? null;
          if (main) {
            try {
              const mainCommits = await httpWorkbenchTransport.git.listCommits(
                worktree.projectId,
                main.id,
                100,
              );
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
    },
    [onRefreshWorktrees],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   unknown 后用户需要 same-id 重新核对，而不是死锁或盲发新 id。
   *
   * Code Logic（这个函数做什么）:
   *   按 unknownKind + worktreeId 取 operation id，再跑 reconcileUnknownMutation。
   */
  const handleRetryReconcile = useCallback(async (): Promise<void> => {
    if (mutationPhase !== 'unknown' || !unknownKindRef.current || !unknownWorktreeIdRef.current) {
      return;
    }
    const kind = unknownKindRef.current;
    const worktreeId = unknownWorktreeIdRef.current;
    const clientOperationId =
      kind === 'remove' ? removeOperationIdRef.current : mergeOperationIdRef.current;
    if (!clientOperationId) return;
    const target = worktrees.find((item) => item.id === worktreeId);
    // remove/merge 成功后 worktree 可能已不在列表；仍用最小 stub 对账。
    const worktreeStub: WorkbenchWorktree =
      target
      ?? ({
        id: worktreeId,
        projectId: project?.id ?? '',
      } as WorkbenchWorktree);
    setActionBusy(`${kind}-${worktreeId}`);
    setError(null);
    try {
      const confirmed = await reconcileUnknownMutation(clientOperationId, worktreeStub, kind);
      const phase = resolveMobileMutationPhase('unknown', confirmed);
      if (phase === 'confirmedSucceeded') {
        removeOperationIdRef.current = null;
        mergeOperationIdRef.current = null;
        unknownKindRef.current = null;
        unknownWorktreeIdRef.current = null;
        setMutationPhase('idle');
        setError(null);
        await onRefreshWorktrees?.({ expectedProjectId: worktreeStub.projectId });
      } else if (phase === 'confirmedFailed') {
        removeOperationIdRef.current = null;
        mergeOperationIdRef.current = null;
        unknownKindRef.current = null;
        unknownWorktreeIdRef.current = null;
        setMutationPhase('idle');
        setError(t('workbench:errors.mutationFailed'));
      } else {
        setMutationPhase('unknown');
        setError(t('workbench:errors.mutationUnknown'));
      }
    } finally {
      setActionBusy(null);
    }
  }, [mutationPhase, onRefreshWorktrees, project?.id, reconcileUnknownMutation, t, worktrees]);

  const handleMergeWorktree = useCallback(
    async (worktree: WorkbenchWorktree): Promise<void> => {
      if (!canRunMobileWorktreeDestructiveAction(worktree, isActionDisabled) && mutationPhase !== 'unknown') {
        return;
      }
      if (mutationPhase === 'unknown' && unknownKindRef.current !== 'merge') return;
      setActionBusy(`merge-${worktree.id}`);
      setMutationPhase('busy');
      setError(null);
      try {
        if (mutationPhase === 'unknown' && mergeOperationIdRef.current) {
          const confirmed = await reconcileUnknownMutation(
            mergeOperationIdRef.current,
            worktree,
            'merge',
          );
          const phase = resolveMobileMutationPhase('unknown', confirmed);
          if (phase === 'confirmedSucceeded') {
            mergeOperationIdRef.current = null;
            unknownKindRef.current = null;
            unknownWorktreeIdRef.current = null;
            setMutationPhase('idle');
            setError(null);
            await onRefreshWorktrees?.({ expectedProjectId: worktree.projectId });
          } else if (phase === 'confirmedFailed') {
            mergeOperationIdRef.current = null;
            unknownKindRef.current = null;
            unknownWorktreeIdRef.current = null;
            setMutationPhase('idle');
            setError(t('workbench:errors.mutationFailed'));
          } else {
            unknownKindRef.current = 'merge';
            unknownWorktreeIdRef.current = worktree.id;
            setMutationPhase('unknown');
            setError(t('workbench:errors.mutationUnknown'));
          }
          return;
        }

        const result = await onMergeWorktree?.(worktree);
        if (result === false) {
          setMutationPhase('idle');
          return;
        }
        mergeOperationIdRef.current = null;
        unknownKindRef.current = null;
        unknownWorktreeIdRef.current = null;
        setMutationPhase('idle');
      } catch (reason) {
        if (isWorkbenchMutationUnknownError(reason)) {
          mergeOperationIdRef.current = getUnknownMutationClientOperationId(reason);
          unknownKindRef.current = 'merge';
          unknownWorktreeIdRef.current = worktree.id;
          setMutationPhase('unknown');
          setError(t('workbench:errors.mutationUnknown'));
        } else {
          setMutationPhase('idle');
          mergeOperationIdRef.current = null;
          unknownKindRef.current = null;
          unknownWorktreeIdRef.current = null;
          setError(`${t('workbench:errors.mergeWorktree')}: ${getErrorMessage(reason)}`);
        }
      } finally {
        setActionBusy(null);
      }
    },
    [isActionDisabled, mutationPhase, onMergeWorktree, onRefreshWorktrees, reconcileUnknownMutation, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户需要能从手机端清理已完成的功能 worktree，但主工作区不能被删除。
   *
   * Code Logic（这个函数做什么）:
   *   删除前 confirm；active 删除先 dirty guard；remove 走 envelope + 稳定 operation id 对账。
   */
  const handleRemoveWorktree = useCallback(
    async (worktree: WorkbenchWorktree): Promise<void> => {
      if (
        !canRunMobileWorktreeDestructiveAction(worktree, isActionDisabled)
        && !(
          mutationPhase === 'unknown'
          && unknownKindRef.current === 'remove'
          && unknownWorktreeIdRef.current === worktree.id
        )
      ) {
        return;
      }
      // unknown same-id 重新核对不再弹 confirm。
      if (mutationPhase !== 'unknown') {
        const shouldRemove = window.confirm(
          t('workbench:mobile.worktreePanel.removeConfirm', { name: worktree.name }),
        );
        if (!shouldRemove) return;
      }

      setActionBusy(`remove-${worktree.id}`);
      setMutationPhase('busy');
      setError(null);
      const clientOperationId = pickMobileMutationOperationId(
        mutationPhase,
        removeOperationIdRef.current,
        createHttpOrchestratorClientRequestId(),
      );
      removeOperationIdRef.current = clientOperationId;

      try {
        if (mutationPhase === 'unknown' && unknownKindRef.current === 'remove') {
          const confirmed = await reconcileUnknownMutation(
            clientOperationId,
            worktree,
            'remove',
          );
          const phase = resolveMobileMutationPhase('unknown', confirmed);
          if (phase === 'confirmedSucceeded') {
            removeOperationIdRef.current = null;
            unknownKindRef.current = null;
            unknownWorktreeIdRef.current = null;
            setMutationPhase('idle');
            setError(null);
            await onRefreshWorktrees?.({
              skipFileContextConfirm: activeWorktreeId === worktree.id,
              expectedProjectId: worktree.projectId,
            });
          } else if (phase === 'confirmedFailed') {
            removeOperationIdRef.current = null;
            unknownKindRef.current = null;
            unknownWorktreeIdRef.current = null;
            setMutationPhase('idle');
            setError(t('workbench:errors.mutationFailed'));
          } else {
            unknownKindRef.current = 'remove';
            unknownWorktreeIdRef.current = worktree.id;
            setMutationPhase('unknown');
            setError(t('workbench:errors.mutationUnknown'));
          }
          return;
        }

        const result = await runMobileWorktreeRemovalFlow({
          worktrees,
          activeWorktreeId,
          removingWorktree: worktree,
          confirmActiveWorktreeChange: (nextActive) =>
            onConfirmActiveWorktreeChange?.(nextActive) ?? true,
          removeWorktree: async () => {
            const endWorktreeOperation = onBeginWorktreeOperation?.();
            try {
              const envelope = await workbenchHttp.git.remove({
                worktreeId: worktree.id,
                force: false,
                clientOperationId,
              });
              if (isMutationSucceeded(envelope)) {
                removeOperationIdRef.current = null;
                unknownKindRef.current = null;
                unknownWorktreeIdRef.current = null;
                setMutationPhase('idle');
                return;
              }
              if (isMutationUnknown(envelope)) {
                const confirmed = await reconcileUnknownMutation(
                  envelope.clientOperationId,
                  worktree,
                  'remove',
                );
                const phase = resolveMobileMutationPhase('unknown', confirmed);
                if (phase === 'confirmedSucceeded') {
                  removeOperationIdRef.current = null;
                  unknownKindRef.current = null;
                  unknownWorktreeIdRef.current = null;
                  setMutationPhase('idle');
                  return;
                }
                if (phase === 'confirmedFailed') {
                  removeOperationIdRef.current = null;
                  unknownKindRef.current = null;
                  unknownWorktreeIdRef.current = null;
                  setMutationPhase('idle');
                  throw new Error(t('workbench:errors.mutationFailed'));
                }
                unknownKindRef.current = 'remove';
                unknownWorktreeIdRef.current = worktree.id;
                setMutationPhase('unknown');
                throw new WorkbenchMutationUnknownError(envelope.clientOperationId);
              }
            } finally {
              endWorktreeOperation?.();
            }
          },
          applyRemoval: (plan) => {
            const sourceBecameActive =
              onIsWorktreeActive?.(worktree) ?? activeWorktreeId === worktree.id;
            onWorktreesChange?.(plan.nextWorktrees);
            if (plan.requiresActivePreflight || sourceBecameActive) {
              onActiveWorktreeChange?.(plan.nextActive);
            }
          },
        });
        if (result === 'applied') {
          await onRefreshWorktrees?.({
            skipFileContextConfirm: activeWorktreeId === worktree.id,
            expectedProjectId: worktree.projectId,
          });
        } else {
          setMutationPhase('idle');
          removeOperationIdRef.current = null;
          unknownKindRef.current = null;
          unknownWorktreeIdRef.current = null;
        }
      } catch (reason) {
        if (isWorkbenchMutationUnknownError(reason) || unknownKindRef.current === 'remove') {
          const opId = getUnknownMutationClientOperationId(reason) ?? removeOperationIdRef.current;
          removeOperationIdRef.current = opId;
          unknownKindRef.current = 'remove';
          unknownWorktreeIdRef.current = worktree.id;
          setMutationPhase('unknown');
          setError(t('workbench:errors.mutationUnknown'));
        } else {
          setMutationPhase('idle');
          removeOperationIdRef.current = null;
          unknownKindRef.current = null;
          unknownWorktreeIdRef.current = null;
          setError(`${t('workbench:errors.removeWorktree')}: ${getErrorMessage(reason)}`);
        }
      } finally {
        setActionBusy(null);
      }
    },
    [
      activeWorktreeId,
      isActionDisabled,
      mutationPhase,
      onActiveWorktreeChange,
      onBeginWorktreeOperation,
      onConfirmActiveWorktreeChange,
      onIsWorktreeActive,
      onRefreshWorktrees,
      onWorktreesChange,
      reconcileUnknownMutation,
      t,
      worktrees,
    ],
  );

  return (
    <section className={styles.panel} aria-labelledby="mobile-worktree-panel-title">
      <div className={styles.panelHeader}>
        <p className={styles.panelKicker}>{t('workbench:mobile.kicker')}</p>
        <h1 id="mobile-worktree-panel-title">{t('workbench:mobile.worktreePanel.title')}</h1>
      </div>

      {!project ? (
        <p className={styles.panelState}>{t('workbench:mobile.worktreePanel.noProject')}</p>
      ) : null}
      {busy ? <p className={styles.panelState}>{t('workbench:loading')}</p> : null}
      {mutationPhase === 'reconciling' ? (
        <StatusMessage tone="info" className={styles.panelState}>
          {t('workbench:mobile.worktreePanel.reconciling')}
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
                {t('workbench:mobile.worktreePanel.retryReconcile')}
              </button>
            ) : undefined
          }
        >
          <span>{t('workbench:mobile.projectPanel.error')}</span>
          <span>{error}</span>
        </StatusMessage>
      ) : null}

      <div className={styles.mobileFormInline}>
        <label className={styles.mobileField}>
          <span>{t('workbench:worktrees.prefixLabel')}</span>
          <select
            className={styles.mobileSelect}
            value={branchPrefix}
            disabled={!project || isActionDisabled}
            onChange={(event) => setBranchPrefix(event.target.value as WorktreeBranchPrefix)}
          >
            {WORKTREE_BRANCH_PREFIXES.map((prefix) => (
              <option key={prefix} value={prefix}>
                {prefix}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.mobileField}>
          <span>{t('workbench:worktrees.suffixLabel')}</span>
          <input
            className={styles.mobileInput}
            value={branchSuffix}
            disabled={!project || isActionDisabled}
            placeholder={t('workbench:worktrees.suffixPlaceholder')}
            onChange={(event) => setBranchSuffix(event.target.value)}
          />
        </label>
        <button
          type="button"
          className={styles.mobileTerminalPrimaryButton}
          disabled={!project || !composedBranchName || isActionDisabled}
          onClick={() => void handleCreateWorktree()}
        >
          {actionBusy === 'create'
            ? t('workbench:mobile.worktreePanel.creating')
            : t('workbench:worktrees.create')}
        </button>
      </div>

      {isEmpty && project ? (
        <p className={styles.panelState}>{t('workbench:worktrees.empty')}</p>
      ) : null}

      <div className={styles.mobileList}>
        {worktrees.map((worktree) => {
          const isActive = worktree.id === activeWorktreeId;
          const statusKind = getMobileWorktreeStatusKind(worktree);
          const statusLabel =
            statusKind === 'conflict'
              ? t('workbench:worktrees.status.conflict', { count: worktree.status.conflicts })
              : statusKind === 'dirty'
                ? t('workbench:worktrees.status.dirty', { count: worktree.status.changed })
                : t('workbench:worktrees.status.clean');
          const syncLabel = `${t('workbench:mobile.worktreePanel.sync')} · ${t(
            'workbench:mobile.gitPanel.aheadBehindValue',
            {
              ahead: worktree.status.ahead,
              behind: worktree.status.behind,
            },
          )}`;
          const canRunDestructiveAction = canRunMobileWorktreeDestructiveAction(
            worktree,
            isActionDisabled,
          );

          return (
            <article
              key={worktree.id}
              className={`${styles.mobileListItem} ${
                isActive ? styles.mobileListItemActive : ''
              }`}
            >
              <button
                type="button"
                className={styles.mobileListItemButton}
                aria-pressed={isActive}
                disabled={isActionDisabled}
                onClick={() => onSelect(worktree)}
              >
                <span className={styles.mobileListTitleRow}>
                  <strong className={styles.mobileListTitle}>{worktree.name}</strong>
                  <span
                    className={`${styles.mobileBadge} ${
                      worktree.isMain ? styles.mobileBadgeAccent : ''
                    }`}
                  >
                    {worktree.isMain
                      ? t('workbench:worktrees.main')
                      : t('workbench:worktrees.linked')}
                  </span>
                </span>
                <span className={styles.mobileListPath}>
                  {worktree.branch ?? t('workbench:emptyValue')}
                </span>
                <span className={styles.mobileListMeta}>{worktree.path}</span>
                <span className={styles.mobileBadgeRow}>
                  <span className={styles.mobileBadge}>{statusLabel}</span>
                  <span className={styles.mobileBadge}>{syncLabel}</span>
                  <span
                    className={`${styles.mobileBadge} ${
                      worktree.status.canPush ? styles.mobileBadgeAccent : ''
                    }`}
                  >
                    {worktree.status.canPush
                      ? t('workbench:mobile.gitPanel.canPushAllowed')
                      : t('workbench:mobile.gitPanel.canPushBlocked')}
                  </span>
                </span>
              </button>
              <div className={styles.mobileToolbar}>
                <button
                  type="button"
                  className={styles.secondaryButton}
                  disabled={!canRunDestructiveAction}
                  onClick={() => void handleMergeWorktree(worktree)}
                >
                  {actionBusy === `merge-${worktree.id}`
                    ? t('workbench:mobile.worktreePanel.merging')
                    : t('workbench:worktrees.merge')}
                </button>
                <button
                  type="button"
                  className={styles.secondaryButton}
                  disabled={!canRunDestructiveAction}
                  onClick={() => void handleRemoveWorktree(worktree)}
                >
                  {actionBusy === `remove-${worktree.id}`
                    ? t('workbench:mobile.worktreePanel.removing')
                    : t('workbench:worktrees.remove')}
                </button>
              </div>
            </article>
          );
        })}
      </div>
    </section>
  );
}
