import { useCallback, useRef } from 'react';
import type { KeyboardEvent, ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Dialog, Input, StatusMessage } from '@/components/primitives';
import { PlusIcon, XIcon } from '@/lib/icons';
import type { WorkbenchWorktree } from '@/lib/types';
import {
  WORKTREE_BRANCH_PREFIXES,
  composeWorktreeBranchName,
  type WorktreeBranchPrefix,
} from '@/lib/workbenchWorktreeBranches';
import { worktreeStatusTone } from '@/pages/Workbench/workbenchWorktrees';
import type { MobileMutationPhase } from '../mobilePanelState';
import { canRunMobileWorktreeDestructiveAction } from '../mobileWorkbenchState';
import { PointerPrimaryButton } from './PointerPrimaryButton';
import styles from './MobileWorktreeTabs.module.css';

export interface MobileWorktreeTabsProps {
  worktrees: WorkbenchWorktree[];
  activeWorktreeId: string | null;
  projectId: string | null;
  /** 项目详情加载或全局 worktree 操作中；不包含终端 pane/session busy。 */
  controlsBusy?: boolean;
  createOpen: boolean;
  createPrefix: WorktreeBranchPrefix;
  createSuffix: string;
  creating?: boolean;
  removing?: boolean;
  pendingRemoval: WorkbenchWorktree | null;
  error: string | null;
  mutationPhase?: MobileMutationPhase;
  onSelect: (worktree: WorkbenchWorktree) => boolean | void;
  onOpenCreate: () => void;
  onCancelCreate: () => void;
  onPrefixChange: (prefix: WorktreeBranchPrefix) => void;
  onSuffixChange: (suffix: string) => void;
  onCreate: () => void;
  onRequestRemove: (worktree: WorkbenchWorktree) => void;
  onCancelRemove: () => void;
  onConfirmRemove: () => void;
  onRetryReconcile?: () => void;
}

/**
 * MobileWorktreeTabs（移动端 worktree 工作区 tab 列表）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端用户需要在窗口 tab 上方直接切换、新建和移除非主 worktree，交互对齐桌面
 *   `WorkbenchWorktreeBar`，避免再绕到独立 Worktrees 面板或顶部 quick switch。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 chip 条（状态点 + 分支名 + 主/worktree 元信息）；非主 chip 带关闭按钮；
 *   末尾「新 worktree」展开 prefix/suffix 表单；移除走共享 Dialog；
 *   触摸走 PointerPrimaryButton，避免 IME 首次 tap 被系统吞掉 click。
 */
export function MobileWorktreeTabs({
  worktrees,
  activeWorktreeId,
  projectId,
  controlsBusy = false,
  createOpen,
  createPrefix,
  createSuffix,
  creating = false,
  removing = false,
  pendingRemoval,
  error,
  mutationPhase = 'idle',
  onSelect,
  onOpenCreate,
  onCancelCreate,
  onPrefixChange,
  onSuffixChange,
  onCreate,
  onRequestRemove,
  onCancelRemove,
  onConfirmRemove,
  onRetryReconcile,
}: MobileWorktreeTabsProps): ReactElement {
  const { t } = useTranslation(['workbench', 'common']);
  const navRef = useRef<HTMLElement | null>(null);
  const composedBranchName = composeWorktreeBranchName(createPrefix, createSuffix);
  const barBusy = controlsBusy || creating || removing || mutationPhase === 'reconciling'
    || mutationPhase === 'unknown' || mutationPhase === 'busy';
  const canCreate = Boolean(projectId) && !barBusy;

  /**
   * Business Logic（为什么需要这个函数）:
   *   与窗口 tab 一样保留键盘循环，方便外接键盘在 chip 间移动焦点。
   *
   * Code Logic（这个函数做什么）:
   *   ArrowLeft/Right/Home/End 在 data-mobile-worktree-chip 间循环 focus。
   */
  const handleKeyDown = useCallback(
    (event: KeyboardEvent<HTMLButtonElement>, currentIndex: number): void => {
      const key = event.key;
      if (key !== 'ArrowLeft' && key !== 'ArrowRight' && key !== 'Home' && key !== 'End') {
        return;
      }
      if (worktrees.length === 0) return;
      event.preventDefault();
      const last = worktrees.length - 1;
      let nextIndex: number;
      if (key === 'Home') nextIndex = 0;
      else if (key === 'End') nextIndex = last;
      else if (key === 'ArrowLeft') nextIndex = currentIndex <= 0 ? last : currentIndex - 1;
      else nextIndex = currentIndex >= last ? 0 : currentIndex + 1;
      const nextChip = navRef.current?.querySelectorAll<HTMLButtonElement>(
        'button[data-mobile-worktree-chip]',
      )[nextIndex];
      nextChip?.focus();
    },
    [worktrees],
  );

  return (
    <div className={styles.mobileWorktreeBar}>
      {error ? (
        <StatusMessage
          tone="danger"
          className={styles.mobileWorktreeError}
          action={
            mutationPhase === 'unknown' && onRetryReconcile ? (
              <Button
                variant="secondary"
                size="sm"
                disabled={creating || removing}
                onClick={() => onRetryReconcile()}
              >
                {t('workbench:mobile.worktreePanel.retryReconcile')}
              </Button>
            ) : undefined
          }
        >
          {error}
        </StatusMessage>
      ) : null}

      <nav
        ref={navRef}
        className={styles.mobileWorktreeStrip}
        aria-label={t('workbench:mobile.worktreeTabs.title')}
        data-busy={barBusy || undefined}
        data-testid="mobile-worktree-tabs"
      >
        {worktrees.length === 0 ? (
          <span className={styles.mobileWorktreeEmpty}>{t('workbench:worktrees.empty')}</span>
        ) : (
          worktrees.map((worktree, index) => {
            const tone = worktreeStatusTone(worktree);
            const isActive = worktree.id === activeWorktreeId;
            const label = worktree.branch ?? worktree.name;
            const meta = worktree.isMain
              ? t('workbench:worktrees.main')
              : t('workbench:worktrees.linked');
            const chip = (
              <PointerPrimaryButton
                type="button"
                data-mobile-worktree-chip
                data-active={isActive || undefined}
                data-tone={tone}
                aria-current={isActive ? 'page' : undefined}
                className={styles.mobileWorktreeChip}
                onPrimary={() => {
                  onSelect(worktree);
                }}
                onKeyDown={(event) => handleKeyDown(event, index)}
              >
                <span className={styles.mobileWorktreeDot} data-tone={tone} aria-hidden="true" />
                <span className={styles.mobileWorktreeChipName} title={label}>
                  {label}
                </span>
                <span className={styles.mobileWorktreeChipMeta}>{meta}</span>
              </PointerPrimaryButton>
            );

            if (worktree.isMain) {
              return <span key={worktree.id}>{chip}</span>;
            }

            const removable = canRunMobileWorktreeDestructiveAction(worktree, barBusy);
            return (
              <div
                key={worktree.id}
                className={styles.mobileWorktreeChipGroup}
                data-active={isActive || undefined}
                data-tone={tone}
                data-removable={removable || undefined}
              >
                {chip}
                <PointerPrimaryButton
                  type="button"
                  className={styles.mobileWorktreeChipClose}
                  data-testid={`mobile-worktree-remove-${worktree.id}`}
                  aria-label={t('workbench:worktrees.removeAria', { name: label })}
                  title={t('workbench:worktrees.removeAria', { name: label })}
                  disabled={!removable}
                  onPrimary={() => {
                    onRequestRemove(worktree);
                  }}
                >
                  <XIcon size={14} />
                </PointerPrimaryButton>
              </div>
            );
          })
        )}

        <div className={styles.mobileWorktreeCreateSlot}>
          {createOpen ? null : (
            <PointerPrimaryButton
              type="button"
              className={styles.mobileWorktreeCreateButton}
              data-testid="mobile-worktree-create"
              disabled={!canCreate}
              onPrimary={() => {
                if (!canCreate) return;
                onOpenCreate();
              }}
            >
              <PlusIcon size={14} aria-hidden="true" />
              <span>{t('workbench:worktrees.create')}</span>
            </PointerPrimaryButton>
          )}
        </div>

        {createOpen ? (
          <form
            className={styles.mobileWorktreeCreateForm}
            data-testid="mobile-worktree-create-form"
            onSubmit={(event) => {
              event.preventDefault();
              if (!composedBranchName || barBusy) return;
              onCreate();
            }}
          >
            <label>
              <span className="sr-only">{t('workbench:worktrees.prefixLabel')}</span>
              <select
                className={styles.mobileWorktreePrefixSelect}
                value={createPrefix}
                disabled={creating}
                aria-label={t('workbench:worktrees.prefixLabel')}
                onChange={(event) =>
                  onPrefixChange(event.target.value as WorktreeBranchPrefix)
                }
              >
                {WORKTREE_BRANCH_PREFIXES.map((prefix) => (
                  <option key={prefix} value={prefix}>
                    {prefix}
                  </option>
                ))}
              </select>
            </label>
            <span className={styles.mobileWorktreeBranchSlash} aria-hidden="true">
              /
            </span>
            <Input
              size="sm"
              mono
              className={styles.mobileWorktreeBranchInput}
              value={createSuffix}
              placeholder={t('workbench:worktrees.suffixPlaceholder')}
              aria-label={t('workbench:worktrees.suffixLabel')}
              disabled={creating}
              onChange={(event) => onSuffixChange(event.target.value)}
            />
            <PointerPrimaryButton
              type="button"
              className={styles.mobileWorktreeCreateButton}
              data-active="true"
              disabled={!composedBranchName || barBusy}
              onPrimary={() => {
                if (!composedBranchName || barBusy) return;
                onCreate();
              }}
            >
              {creating
                ? t('workbench:mobile.worktreePanel.creating')
                : t('common:action.confirm')}
            </PointerPrimaryButton>
            <PointerPrimaryButton
              type="button"
              className={styles.mobileWorktreeCreateButton}
              disabled={creating}
              onPrimary={() => {
                if (creating) return;
                onCancelCreate();
              }}
            >
              {t('common:action.cancel')}
            </PointerPrimaryButton>
          </form>
        ) : null}
      </nav>

      <Dialog
        open={pendingRemoval !== null}
        titleId="mobile-worktree-remove-confirm-title"
        closeOnEscape={!removing}
        closeOnBackdrop={!removing}
        onClose={() => {
          if (removing) return;
          onCancelRemove();
        }}
      >
        <h2
          id="mobile-worktree-remove-confirm-title"
          className={styles.mobileWorktreeRemoveTitle}
        >
          {t('workbench:worktrees.removeConfirmDialog.title')}
        </h2>
        <p className={styles.mobileWorktreeRemoveBody}>
          {t('workbench:worktrees.removeConfirmDialog.body', {
            name: pendingRemoval?.branch ?? pendingRemoval?.name ?? '',
          })}
        </p>
        <div className={styles.mobileWorktreeRemoveActions}>
          <Button
            variant="ghost"
            size="sm"
            disabled={removing}
            onClick={onCancelRemove}
          >
            {t('workbench:worktrees.removeConfirmDialog.cancel')}
          </Button>
          <Button
            variant="danger"
            size="sm"
            loading={removing}
            disabled={removing || !pendingRemoval}
            onClick={onConfirmRemove}
          >
            {t('workbench:worktrees.removeConfirmDialog.confirm')}
          </Button>
        </div>
      </Dialog>
    </div>
  );
}
