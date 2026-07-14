import { useCallback, useId } from 'react';
import type { ReactElement } from 'react';
import type { TFunction } from 'i18next';
import { Dialog } from '@/components/primitives';
import type { WorkbenchProject, WorkbenchWorktree } from '@/lib/types';
import {
  getMobileWorktreeStatusKind,
  type MobileWorkbenchPanel,
  type MobileWorktreeStatusKind,
} from '../mobileWorkbenchState';
import styles from '../MobileWorkbench.module.css';

export interface MobileWorktreeQuickSwitchProps {
  open: boolean;
  project: WorkbenchProject | null;
  worktrees: WorkbenchWorktree[];
  activeWorktreeId: string | null;
  busy?: boolean;
  t: TFunction<'workbench'>;
  onClose: () => void;
  onSelect: (worktree: WorkbenchWorktree) => boolean | void;
  onPanelChange: (panel: MobileWorkbenchPanel) => void;
  onRefresh: () => Promise<void> | void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   快速切换 sheet 与完整 worktree 面板需要共享移动端状态分类，避免 clean/dirty/conflict 文案不一致。
 *
 * Code Logic（这个函数做什么）:
 *   接收 worktree 与 workbench namespace 的 t 函数，按移动端状态 kind 返回本地化状态文案。
 */
function getStatusLabel(worktree: WorkbenchWorktree, t: TFunction<'workbench'>): string {
  const statusKind = getMobileWorktreeStatusKind(worktree);
  if (statusKind === 'conflict') {
    return t('worktrees.status.conflict', { count: worktree.status.conflicts });
  }
  if (statusKind === 'dirty') {
    return t('worktrees.status.dirty', { count: worktree.status.changed });
  }
  return t('worktrees.status.clean');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   worktree 状态 badge 需要跟随 clean/dirty/conflict 变化呈现不同视觉状态，帮助手机端快速扫读。
 *
 * Code Logic（这个函数做什么）:
 *   接收移动端状态 kind，返回 CSS Module 中对应的状态修饰类。
 */
function getStatusClassName(statusKind: MobileWorktreeStatusKind): string {
  if (statusKind === 'conflict') return styles.quickSwitchStatusConflict;
  if (statusKind === 'dirty') return styles.quickSwitchStatusDirty;
  return styles.quickSwitchStatusClean;
}

/**
 * MobileWorktreeQuickSwitch（移动端 worktree 快速切换 sheet）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端用户在终端、文件和 Git 面板之间工作时，需要从顶部 worktree pill 快速查看、刷新并切换当前 worktree。
 *
 * Code Logic（这个组件做什么）:
 *   以共享 Dialog 原语渲染 modal bottom sheet（portal / Escape / backdrop / focus trap），
 *   展示 worktree 列表、主工作区/状态 badge，并提供刷新、进入完整管理面板和关闭操作。
 *   hooks 全部在 return 前；open=false 时由 Dialog 返回 null。
 */
export function MobileWorktreeQuickSwitch({
  open,
  project,
  worktrees,
  activeWorktreeId,
  busy = false,
  t,
  onClose,
  onSelect,
  onPanelChange,
  onRefresh,
}: MobileWorktreeQuickSwitchProps): ReactElement | null {
  const titleId = useId();
  const isEmpty = project !== null && worktrees.length === 0;

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在 quick switch 中点击刷新时，只应触发列表刷新，不改变当前面板或关闭 sheet。
   *
   * Code Logic（这个函数做什么）:
   *   busy 时忽略点击；否则调用父级 onRefresh，并允许其自行处理异步结果。
   */
  const handleRefresh = useCallback((): void => {
    if (busy) return;
    void onRefresh();
  }, [busy, onRefresh]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   快速切换器只提供轻量入口，创建/删除/merge 等完整操作应交给 worktrees 管理面板。
   *
   * Code Logic（这个函数做什么）:
   *   busy 时忽略点击；否则切换到 worktrees panel 后关闭当前 sheet。
   */
  const handleManage = useCallback((): void => {
    if (busy) return;
    onPanelChange('worktrees');
    onClose();
  }, [busy, onClose, onPanelChange]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户选择 worktree 后需要尊重父级 dirty guard；父级拒绝切换时 sheet 应保留以便继续操作。
   *
   * Code Logic（这个函数做什么）:
   *   调用 onSelect，只有返回 false 时保持打开，其它返回值都视为接受并关闭 sheet。
   */
  const handleSelect = useCallback(
    (worktree: WorkbenchWorktree): void => {
      if (busy) return;
      const accepted = onSelect(worktree);
      if (accepted !== false) {
        onClose();
      }
    },
    [busy, onClose, onSelect],
  );

  return (
    <Dialog open={open} titleId={titleId} onClose={onClose} className={styles.quickSwitchSheet}>
      <header className={styles.quickSwitchHeader}>
        <div className={styles.quickSwitchTitle}>
          <p>{t('mobile.kicker')}</p>
          <h2 id={titleId}>
            {t('mobile.worktreeQuickSwitch.title', { project: project?.name ?? '' })}
          </h2>
        </div>
        <div className={styles.quickSwitchActions}>
          <button
            type="button"
            className={styles.quickSwitchActionButton}
            disabled={busy}
            onClick={handleRefresh}
          >
            {t('mobile.worktreeQuickSwitch.refresh')}
          </button>
          <button
            type="button"
            className={styles.quickSwitchActionButton}
            disabled={busy}
            onClick={handleManage}
          >
            {t('mobile.worktreeQuickSwitch.manage')}
          </button>
          <button type="button" className={styles.quickSwitchCloseButton} onClick={onClose}>
            {t('mobile.worktreeQuickSwitch.close')}
          </button>
        </div>
      </header>

      {!project ? (
        <p className={styles.panelState}>{t('mobile.worktreePanel.noProject')}</p>
      ) : null}
      {isEmpty ? <p className={styles.panelState}>{t('worktrees.empty')}</p> : null}

      {project ? (
        <div className={styles.quickSwitchList}>
          {worktrees.map((worktree) => {
            const isActive = worktree.id === activeWorktreeId;
            const statusKind = getMobileWorktreeStatusKind(worktree);
            const itemClassName = `${styles.quickSwitchItem} ${
              isActive ? styles.quickSwitchItemActive : ''
            }`;
            const statusClassName = `${styles.quickSwitchStatus} ${getStatusClassName(statusKind)}`;

            return (
              <button
                key={worktree.id}
                type="button"
                className={itemClassName}
                aria-current={isActive ? 'true' : undefined}
                disabled={busy}
                onClick={() => handleSelect(worktree)}
              >
                <span className={styles.quickSwitchItemHeader}>
                  <strong>{worktree.name}</strong>
                  <span className={styles.quickSwitchBadge}>
                    {worktree.isMain ? t('worktrees.main') : t('worktrees.linked')}
                  </span>
                </span>
                <span className={styles.quickSwitchMeta}>
                  {worktree.branch ?? t('emptyValue')}
                </span>
                <span className={styles.quickSwitchPath}>{worktree.path}</span>
                <span className={statusClassName}>{getStatusLabel(worktree, t)}</span>
              </button>
            );
          })}
        </div>
      ) : null}
    </Dialog>
  );
}
