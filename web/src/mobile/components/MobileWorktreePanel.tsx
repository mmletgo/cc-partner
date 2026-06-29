import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import type { WorkbenchWorktree } from '@/lib/types';
import styles from '../MobileWorkbench.module.css';

export interface MobileWorktreePanelProps {
  worktrees: WorkbenchWorktree[];
  activeWorktreeId: string | null;
  onSelect: (worktree: WorkbenchWorktree) => void;
}

/**
 * MobileWorktreePanel（移动端 worktree 列表面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   移动端用户需要切换 active worktree，驱动终端、文件和 Git 面板使用同一个工作区上下文。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 worktree 名称、分支、主工作区标识和 Git 状态摘要，并把选择事件交给父组件更新 active worktree/session。
 */
export function MobileWorktreePanel({
  worktrees,
  activeWorktreeId,
  onSelect,
}: MobileWorktreePanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const isEmpty = worktrees.length === 0;

  return (
    <section className={styles.panel} aria-labelledby="mobile-worktree-panel-title">
      <div className={styles.panelHeader}>
        <p className={styles.panelKicker}>{t('workbench:mobile.kicker')}</p>
        <h1 id="mobile-worktree-panel-title">{t('workbench:worktrees.label')}</h1>
      </div>

      {isEmpty ? <p className={styles.panelState}>{t('workbench:worktrees.empty')}</p> : null}

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
            <button
              key={worktree.id}
              type="button"
              className={`${styles.mobileListItem} ${
                isActive ? styles.mobileListItemActive : ''
              }`}
              aria-pressed={isActive}
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
          );
        })}
      </div>
    </section>
  );
}
