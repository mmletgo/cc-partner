import { useCallback, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import type { WorkbenchProject, WorkbenchWorktree } from '@/lib/types';
import {
  DEFAULT_WORKTREE_BRANCH_PREFIX,
  WORKTREE_BRANCH_PREFIXES,
  composeWorktreeBranchName,
  type WorktreeBranchPrefix,
} from '@/lib/workbenchWorktreeBranches';
import { runMobileWorktreeRemovalFlow } from '../mobilePanelState';
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
 *
 * Code Logic（这个组件做什么）:
 *   渲染 worktree 名称、分支、路径、状态、同步和推送摘要；同时提供 prefix/suffix 创建、merge 和删除非主 worktree 的操作入口。
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
  const [error, setError] = useState<string | null>(null);
  const composedBranchName = composeWorktreeBranchName(branchPrefix, branchSuffix);
  const isEmpty = worktrees.length === 0;
  const isActionDisabled = busy || actionBusy !== null;

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
   *   非主且非 busy 时调用父级 merge flow；父级负责成功后的列表刷新，避免子级重复刷新把成功操作误报为失败。
   */
  const handleMergeWorktree = useCallback(
    async (worktree: WorkbenchWorktree): Promise<void> => {
      if (!canRunMobileWorktreeDestructiveAction(worktree, isActionDisabled)) return;
      setActionBusy(`merge-${worktree.id}`);
      setError(null);
      try {
        await onMergeWorktree?.(worktree);
      } catch (reason) {
        setError(`${t('workbench:errors.mergeWorktree')}: ${getErrorMessage(reason)}`);
      } finally {
        setActionBusy(null);
      }
    },
    [isActionDisabled, onMergeWorktree, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户需要能从手机端清理已完成的功能 worktree，但主工作区不能被删除。
   *
   * Code Logic（这个函数做什么）:
   *   删除前使用 window.confirm 二次确认；active 删除先做只读 dirty guard，后端删除成功后才更新列表与 active worktree。
   */
  const handleRemoveWorktree = useCallback(
    async (worktree: WorkbenchWorktree): Promise<void> => {
      if (!canRunMobileWorktreeDestructiveAction(worktree, isActionDisabled)) return;
      const shouldRemove = window.confirm(
        t('workbench:mobile.worktreePanel.removeConfirm', { name: worktree.name }),
      );
      if (!shouldRemove) return;

      setActionBusy(`remove-${worktree.id}`);
      setError(null);
      try {
        const result = await runMobileWorktreeRemovalFlow({
          worktrees,
          activeWorktreeId,
          removingWorktree: worktree,
          confirmActiveWorktreeChange: (nextActive) =>
            onConfirmActiveWorktreeChange?.(nextActive) ?? true,
          removeWorktree: async () => {
            const endWorktreeOperation = onBeginWorktreeOperation?.();
            try {
              await httpWorkbenchTransport.worktrees.remove(worktree.id, false);
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
        }
      } catch (reason) {
        setError(`${t('workbench:errors.removeWorktree')}: ${getErrorMessage(reason)}`);
      } finally {
        setActionBusy(null);
      }
    },
    [
      activeWorktreeId,
      onActiveWorktreeChange,
      onBeginWorktreeOperation,
      onConfirmActiveWorktreeChange,
      onRefreshWorktrees,
      onIsWorktreeActive,
      onWorktreesChange,
      t,
      worktrees,
      isActionDisabled,
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
      {error ? (
        <p className={styles.panelError}>
          <span>{t('workbench:mobile.projectPanel.error')}</span>
          <span>{error}</span>
        </p>
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
