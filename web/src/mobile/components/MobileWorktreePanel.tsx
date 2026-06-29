import { useCallback, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import type { WorkbenchProject, WorkbenchWorktree } from '@/lib/types';
import { selectPreferredMobileWorktree } from '../mobileWorkbenchState';
import styles from '../MobileWorkbench.module.css';

export interface MobileWorktreePanelProps {
  project: WorkbenchProject | null;
  worktrees: WorkbenchWorktree[];
  activeWorktreeId: string | null;
  busy?: boolean;
  onSelect: (worktree: WorkbenchWorktree) => boolean | void;
  onWorktreesChange?: (worktrees: WorkbenchWorktree[]) => void;
  onActiveWorktreeChange?: (worktree: WorkbenchWorktree | null) => boolean | void;
  onRefreshWorktrees?: () => Promise<void> | void;
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
 *   移动端用户需要切换 active worktree，驱动终端、文件和 Git 面板使用同一个工作区上下文。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 worktree 名称、分支、主工作区标识和 Git 状态摘要；同时提供创建和删除非主 worktree 的 HTTP 操作入口。
 */
export function MobileWorktreePanel({
  project,
  worktrees,
  activeWorktreeId,
  busy = false,
  onSelect,
  onWorktreesChange,
  onActiveWorktreeChange,
  onRefreshWorktrees,
}: MobileWorktreePanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const [branchName, setBranchName] = useState<string>('');
  const [actionBusy, setActionBusy] = useState<'create' | string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const isEmpty = worktrees.length === 0;
  const isActionDisabled = busy || actionBusy !== null;

  /**
   * Business Logic（为什么需要这个函数）:
   *   创建或删除 worktree 后，父组件需要同步列表，并在父级允许时同步 active worktree。
   *
   * Code Logic（这个函数做什么）:
   *   调用父级列表回调；active 非空时复用既有 onSelect，空 active 才走 onActiveWorktreeChange，并把父级是否接受切换转换为 boolean。
   */
  const applyWorktrees = useCallback(
    (nextWorktrees: WorkbenchWorktree[], nextActive: WorkbenchWorktree | null): boolean => {
      onWorktreesChange?.(nextWorktrees);
      if (nextActive) {
        return onSelect(nextActive) !== false;
      }
      return onActiveWorktreeChange?.(null) !== false;
    },
    [onActiveWorktreeChange, onSelect, onWorktreesChange],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端需要能从当前项目创建一个功能 worktree，供后续终端、文件和 Git 面板共同使用。
   *
   * Code Logic（这个函数做什么）:
   *   读取表单分支名，调用 worktrees.create(projectId, branchName, null)，成功后追加到列表并设为 active。
   */
  const handleCreateWorktree = useCallback(async (): Promise<void> => {
    const trimmedBranchName = branchName.trim();
    if (!project || !trimmedBranchName) return;
    setActionBusy('create');
    setError(null);
    try {
      const created = await httpWorkbenchTransport.worktrees.create(
        project.id,
        trimmedBranchName,
        null,
      );
      const nextWorktrees = [...worktrees.filter((item) => item.id !== created.id), created];
      const didApplyActive = applyWorktrees(nextWorktrees, created);
      setBranchName('');
      if (didApplyActive) {
        await onRefreshWorktrees?.();
      }
    } catch (reason) {
      setError(`${t('workbench:errors.createWorktree')}: ${getErrorMessage(reason)}`);
    } finally {
      setActionBusy(null);
    }
  }, [applyWorktrees, branchName, onRefreshWorktrees, project, t, worktrees]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户需要能从手机端清理已完成的功能 worktree，但主工作区不能被删除。
   *
   * Code Logic（这个函数做什么）:
   *   删除前使用 window.confirm 二次确认；调用 worktrees.remove(force=false)，成功后从列表移除并选择主工作区或首项。
   */
  const handleRemoveWorktree = useCallback(
    async (worktree: WorkbenchWorktree): Promise<void> => {
      if (worktree.isMain) return;
      const shouldRemove = window.confirm(
        t('workbench:mobile.worktreePanel.removeConfirm', { name: worktree.name }),
      );
      if (!shouldRemove) return;

      setActionBusy(`remove-${worktree.id}`);
      setError(null);
      try {
        await httpWorkbenchTransport.worktrees.remove(worktree.id, false);
        const nextWorktrees = worktrees.filter((item) => item.id !== worktree.id);
        const nextActive =
          activeWorktreeId === worktree.id
            ? selectPreferredMobileWorktree(nextWorktrees)
            : worktrees.find((item) => item.id === activeWorktreeId) ?? null;
        const didApplyActive = applyWorktrees(nextWorktrees, nextActive);
        if (didApplyActive) {
          await onRefreshWorktrees?.();
        }
      } catch (reason) {
        setError(`${t('workbench:errors.removeWorktree')}: ${getErrorMessage(reason)}`);
      } finally {
        setActionBusy(null);
      }
    },
    [activeWorktreeId, applyWorktrees, onRefreshWorktrees, t, worktrees],
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
          <span>{t('workbench:mobile.worktreePanel.branchLabel')}</span>
          <input
            className={styles.mobileInput}
            value={branchName}
            disabled={!project || isActionDisabled}
            placeholder={t('workbench:mobile.worktreePanel.branchPlaceholder')}
            onChange={(event) => setBranchName(event.target.value)}
          />
        </label>
        <button
          type="button"
          className={styles.mobileTerminalPrimaryButton}
          disabled={!project || !branchName.trim() || isActionDisabled}
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
          const statusLabel =
            worktree.status.conflicts > 0
              ? t('workbench:worktrees.status.conflict', { count: worktree.status.conflicts })
              : worktree.status.changed > 0
                ? t('workbench:worktrees.status.dirty', { count: worktree.status.changed })
                : t('workbench:worktrees.status.clean');

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
                <span className={styles.mobileListMeta}>{statusLabel}</span>
              </button>
              <div className={styles.mobileToolbar}>
                <button
                  type="button"
                  className={styles.secondaryButton}
                  disabled={worktree.isMain || isActionDisabled}
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
