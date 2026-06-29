import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import type { WorkbenchGitCommit, WorkbenchProject, WorkbenchWorktree } from '@/lib/types';
import {
  shouldReloadMobileGitCommitsAfterAction,
  type MobileGitPanelAction,
} from '../mobilePanelState';
import styles from '../MobileWorkbench.module.css';

export interface MobileGitPanelProps {
  project: WorkbenchProject | null;
  worktree: WorkbenchWorktree | null;
  onWorktreeChange?: (worktree: WorkbenchWorktree) => void;
  onRefreshWorktrees?: () => Promise<void> | void;
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
 *
 * Code Logic（这个组件做什么）:
 *   通过 HTTP transport 加载 commit 列表和调用 worktree Git 操作；使用 request id 防止旧 worktree 响应覆盖当前 UI。
 */
export function MobileGitPanel({
  project,
  worktree,
  onWorktreeChange,
  onRefreshWorktrees,
}: MobileGitPanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const [commits, setCommits] = useState<WorkbenchGitCommit[]>([]);
  const [loading, setLoading] = useState<boolean>(false);
  const [actionBusy, setActionBusy] = useState<'commit' | 'push' | 'merge' | null>(null);
  const [error, setError] = useState<string | null>(null);
  const requestIdRef = useRef<number>(0);
  const statusLabel = useMemo(() => {
    if (!worktree) return t('workbench:mobile.gitPanel.noWorktree');
    if (worktree.status.conflicts > 0) {
      return t('workbench:worktrees.status.conflict', { count: worktree.status.conflicts });
    }
    if (worktree.status.changed > 0) {
      return t('workbench:worktrees.status.dirty', { count: worktree.status.changed });
    }
    return t('workbench:worktrees.status.clean');
  }, [t, worktree]);

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
  }, [project, t, worktree?.id]);

  useEffect(() => {
    void loadCommits();
    return () => {
      requestIdRef.current += 1;
    };
  }, [loadCommits]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   Git 操作完成后父组件需要同步最新 worktree 状态；merge 后源 worktree 可能被删除，不能再用旧 id 刷新提交历史。
   *
   * Code Logic（这个函数做什么）:
   *   commit/push 后刷新 worktree 与 commits；merge 后让旧 commits 请求失效、清空当前提交，再只刷新 worktrees。
   */
  const refreshAfterAction = useCallback(async (action: MobileGitPanelAction): Promise<void> => {
    if (!shouldReloadMobileGitCommitsAfterAction(action)) {
      requestIdRef.current += 1;
      setCommits([]);
      setError(null);
      setLoading(false);
      await onRefreshWorktrees?.();
      return;
    }
    await onRefreshWorktrees?.();
    await loadCommits();
  }, [loadCommits, onRefreshWorktrees]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端需要触发后端 Claude Code 生成提交信息并提交当前 worktree 改动。
   *
   * Code Logic（这个函数做什么）:
   *   调用 worktrees.commit(worktreeId, null)，成功后通知父组件更新 active worktree 并刷新列表/提交历史。
   */
  const handleCommit = useCallback(async (): Promise<void> => {
    if (!worktree) return;
    setActionBusy('commit');
    setError(null);
    try {
      const nextWorktree = await httpWorkbenchTransport.worktrees.commit(worktree.id, null);
      onWorktreeChange?.(nextWorktree);
      await refreshAfterAction('commit');
    } catch (reason) {
      setError(`${t('workbench:errors.commitWorktree')}: ${getErrorMessage(reason)}`);
    } finally {
      setActionBusy(null);
    }
  }, [onWorktreeChange, refreshAfterAction, t, worktree]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   当前 worktree 有可推送分支时，手机端需要能把本地提交推送到后端判定的远端。
   *
   * Code Logic（这个函数做什么）:
   *   调用 worktrees.push，成功后同步 active worktree 状态并刷新 worktree/commit 数据。
   */
  const handlePush = useCallback(async (): Promise<void> => {
    if (!worktree) return;
    setActionBusy('push');
    setError(null);
    try {
      const nextWorktree = await httpWorkbenchTransport.worktrees.push(worktree.id);
      onWorktreeChange?.(nextWorktree);
      await refreshAfterAction('push');
    } catch (reason) {
      setError(`${t('workbench:errors.pushWorktree')}: ${getErrorMessage(reason)}`);
    } finally {
      setActionBusy(null);
    }
  }, [onWorktreeChange, refreshAfterAction, t, worktree]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   功能 worktree 完成后，手机端需要能触发合并回主工作区，沿用后端既有 merge 流程。
   *
   * Code Logic（这个函数做什么）:
   *   调用 worktrees.merge；完成后刷新父级 worktree 列表并清空当前提交，不再用可能已删除的源 worktree 拉提交历史。
   */
  const handleMerge = useCallback(async (): Promise<void> => {
    if (!worktree || worktree.isMain) return;
    setActionBusy('merge');
    setError(null);
    try {
      await httpWorkbenchTransport.worktrees.merge(worktree.id);
      await refreshAfterAction('merge');
    } catch (reason) {
      setError(`${t('workbench:errors.mergeWorktree')}: ${getErrorMessage(reason)}`);
    } finally {
      setActionBusy(null);
    }
  }, [refreshAfterAction, t, worktree]);

  const actionDisabled = actionBusy !== null || !worktree;

  return (
    <section className={styles.panel} aria-labelledby="mobile-git-panel-title">
      <div className={styles.panelHeaderRow}>
        <div className={styles.panelHeader}>
          <p className={styles.panelKicker}>{t('workbench:mobile.kicker')}</p>
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
      {error ? (
        <p className={styles.panelError}>
          <span>{t('workbench:mobile.projectPanel.error')}</span>
          <span>{error}</span>
        </p>
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
            onClick={() => void handlePush()}
          >
            {actionBusy === 'push'
              ? t('workbench:mobile.gitPanel.pushing')
              : t('workbench:worktrees.push')}
          </button>
          <button
            type="button"
            className={styles.secondaryButton}
            disabled={actionDisabled || worktree?.isMain}
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
