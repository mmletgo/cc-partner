/**
 * Workbench worktree 切换条叶子视图 —— worktree chip 列表 + 创建表单 + 工作区切换槽位。
 *
 * Business Logic（为什么需要这个组件）:
 *   Plan 2 Task 8 把 Workbench.tsx centerPane 顶部的 worktreeBar 内部 JSX 抽到独立叶子组件，便于页面降到 ≤1200 行。
 *   外层 `<section ... hidden={automationConsoleOpen}>` 仍保留在 Workbench.tsx（workbenchAutomationView 静态契约
 *   断言该字面量出现在页面源码中），本组件只负责 section 内部的 worktree 切换 + 创建表单 + 工作区切换槽位。
 *
 *   2026-08-15 调整：非主 worktree chip 各自挂 x 按钮触发移除（弹共享 Dialog 二次确认，不再内嵌 trash）。
 *   工作区三元切换（终端 / 网页浏览 / 文件浏览）由 `workspaceSwitch` slot 注入到 tab 行最右侧。
 *   「新建」按钮文案改为「新 worktree」。
 *
 * Code Logic（这个组件做什么）:
 *   - 渲染 worktree chip 列表：主工作区为单 button；非主为 div > [text button(active 切换), x button(移除入口)];
 *   - 渲染 create worktree 表单（prefix select + suffix input + confirm/cancel）或「新 worktree」按钮；
 *   - 渲染 `workspaceSwitch` slot（由父级传入 `WorkbenchWorkspaceSwitch`），不再自带删除按钮；
 *   - 暴露 WorkbenchWorktreeBarProps 类型，所有数据均来自 useWorkbenchWorktreeGitController + Workbench.tsx 跨域共享。
 */
import { useTranslation } from 'react-i18next';
import type { ReactElement, ReactNode } from 'react';
import { Button, HintStatusDot, Input } from '@/components/primitives';
import { useOptionalWorkbenchAgentHints } from '@/hooks/workbenchAgentHintsContext';
import { EMPTY_HINT_COUNTS } from '@/lib/workbenchAgentHints';
import { agentHintAriaSpec } from './workbenchAgentHintPresentation';
import { XIcon, PlusIcon } from '@/lib/icons';
import type { WorkbenchWorktree } from '@/lib/types';
import styles from './Workbench.module.css';
import {
  canRemoveWorktree,
  composeWorktreeBranchName,
  WORKTREE_BRANCH_PREFIXES,
  worktreeStatusTone,
} from './workbenchWorktrees';
import type { WorktreeBranchPrefix } from './workbenchWorktrees';
import type {
  WorktreeBusyKind,
  WorktreeUnknownMutationLock,
} from './controllers/useWorkbenchWorktreeGitController';

/**
 * worktree 切换条叶子组件的输入 props。
 *
 * Business Logic: 所有数据均由 useWorkbenchWorktreeGitController + Workbench.tsx 跨域共享派生；
 * 组件本身不持有状态、不调用 workbenchApi。worktreeBranchInputRef 由页面持有并通过 forwardRef 注入到 suffix Input，
 * 因为 createWorktreeOpen 状态变化时页面需要 focus 该输入。
 *
 * Code Logic: onSelectWorktree 是页面注入的 dirty-context-aware 切换入口——切换前会通过
 * fileController.guardDirtyContextChange 询问用户是否放弃未保存编辑。创建流程的 setActiveWorktreeId
 * 由 controller 内部走另一条路径（不经过本叶子组件）。
 */
export interface WorkbenchWorktreeBarProps {
  worktrees: WorkbenchWorktree[];
  activeWorktree: WorkbenchWorktree | null;
  activeProjectId: string | null;
  remoteWriteDisabled: boolean;
  worktreeBusy: WorktreeBusyKind | null;
  /** unknown 共享锁；禁用 sibling remove Fresh claim。 */
  unknownMutationLock: WorktreeUnknownMutationLock | null;
  createWorktreeOpen: boolean;
  createWorktreeBranchPrefix: WorktreeBranchPrefix;
  createWorktreeBranchSuffixDraft: string;
  worktreeBranchInputRef: React.RefObject<HTMLInputElement | null>;
  /** dirty-context-aware 切换入口；用户在 chip 上点击时调用。 */
  onSelectWorktree: (id: string) => void;
  setCreateWorktreeBranchPrefix: (prefix: WorktreeBranchPrefix) => void;
  setCreateWorktreeBranchSuffixDraft: (suffix: string) => void;
  handleOpenCreateWorktree: () => void;
  handleCancelCreateWorktree: () => void;
  handleCreateWorktree: () => Promise<void>;
  /**
   * 非主 worktree chip 上的 x 按钮触发；页面层据此打开共享 Dialog 二次确认。
   * 移除流程的 controller 调用由 Dialog 确认后由页面发起，本组件不直接调 controller。
   */
  onRequestRemoveWorktree: (worktreeId: string) => void;
  /** 工作区三元切换（终端 / 网页浏览 / 文件浏览）；父级注入 `WorkbenchWorkspaceSwitch`。 */
  workspaceSwitch: ReactNode;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench centerPane 顶部需要稳定的 worktree 切换/创建条，让用户在多 worktree 项目里快速切换上下文；
 *   工作区三元切换作为右对齐槽位与 tab 行共享同一行，避免被其他按钮挤住。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 worktreeStrip（chip 列表 + 新 worktree 按钮末尾）+ workspaceSwitch slot（最右）；不持有状态、不调用 workbenchApi。
 */
export function WorkbenchWorktreeBar(props: WorkbenchWorktreeBarProps): ReactElement {
  const { t } = useTranslation(['workbench', 'common']);
  const hintContext = useOptionalWorkbenchAgentHints();
  const {
    worktrees,
    activeWorktree,
    activeProjectId,
    remoteWriteDisabled,
    worktreeBusy,
    unknownMutationLock,
    createWorktreeOpen,
    createWorktreeBranchPrefix,
    createWorktreeBranchSuffixDraft,
    worktreeBranchInputRef,
    onSelectWorktree,
    setCreateWorktreeBranchPrefix,
    setCreateWorktreeBranchSuffixDraft,
    handleOpenCreateWorktree,
    handleCancelCreateWorktree,
    handleCreateWorktree,
    onRequestRemoveWorktree,
    workspaceSwitch,
  } = props;

  const composedWorktreeBranchName = composeWorktreeBranchName(
    createWorktreeBranchPrefix,
    createWorktreeBranchSuffixDraft,
  );

  return (
    <div className={styles.worktreeStrip}>
      {worktrees.length === 0 ? (
        <span className={styles.worktreeEmpty}>{t('workbench:worktrees.empty')}</span>
      ) : (
        worktrees.map((worktree) => {
          const tone = worktreeStatusTone(worktree);
          const label = worktree.branch ?? worktree.name;
          const hint =
            activeProjectId && hintContext
              ? hintContext.hintsForWorktree(activeProjectId, worktree.id)
              : EMPTY_HINT_COUNTS;
          const hintAria = agentHintAriaSpec(hint);
          const meta = worktree.isMain
            ? t('workbench:worktrees.main')
            : t('workbench:worktrees.linked');
          const isActive = worktree.id === activeWorktree?.id;
          if (worktree.isMain) {
            return (
              <button
                key={worktree.id}
                type="button"
                className={styles.worktreeChip}
                data-active={isActive || undefined}
                data-tone={tone}
                onClick={() => onSelectWorktree(worktree.id)}
              >
                <HintStatusDot
                  className={styles.worktreeDot}
                  data-tone={tone}
                  waitingCount={hint.waitingCount}
                  stoppedCount={hint.stoppedCount}
                  aria-label={t(hintAria.key, hintAria.values)}
                />
                <span className={styles.worktreeName}>{label}</span>
                <span className={styles.worktreeMeta}>{meta}</span>
              </button>
            );
          }
          const removable = canRemoveWorktree(worktree, worktreeBusy, unknownMutationLock)
            && !remoteWriteDisabled;
          return (
            <div
              key={worktree.id}
              className={styles.worktreeChipGroup}
              data-active={isActive || undefined}
              data-tone={tone}
              data-removable={removable || undefined}
            >
              <button
                type="button"
                className={styles.worktreeChip}
                data-active={isActive || undefined}
                data-tone={tone}
                onClick={() => onSelectWorktree(worktree.id)}
              >
                <HintStatusDot
                  className={styles.worktreeDot}
                  data-tone={tone}
                  waitingCount={hint.waitingCount}
                  stoppedCount={hint.stoppedCount}
                  aria-label={t(hintAria.key, hintAria.values)}
                />
                <span className={styles.worktreeName}>{label}</span>
                <span className={styles.worktreeMeta}>{meta}</span>
              </button>
              <button
                type="button"
                className={styles.worktreeChipClose}
                aria-label={t('workbench:worktrees.removeAria', { name: label })}
                title={t('workbench:worktrees.removeAria', { name: label })}
                disabled={!removable}
                onClick={() => onRequestRemoveWorktree(worktree.id)}
              >
                <XIcon />
              </button>
            </div>
          );
        })
      )}
      <div className={styles.worktreeCreateSlot}>
        {createWorktreeOpen ? (
          <form
            className={styles.worktreeCreateForm}
            onSubmit={(event) => {
              event.preventDefault();
              void handleCreateWorktree();
            }}
          >
            <label className={styles.worktreePrefixField}>
              <span className={styles.srOnly}>{t('workbench:worktrees.prefixLabel')}</span>
              <select
                className={styles.worktreePrefixSelect}
                value={createWorktreeBranchPrefix}
                disabled={worktreeBusy === 'create' || remoteWriteDisabled}
                aria-label={t('workbench:worktrees.prefixLabel')}
                onChange={(event) =>
                  setCreateWorktreeBranchPrefix(event.target.value as WorktreeBranchPrefix)
                }
              >
                {WORKTREE_BRANCH_PREFIXES.map((prefix) => (
                  <option key={prefix} value={prefix}>{prefix}</option>
                ))}
              </select>
            </label>
            <span className={styles.worktreeBranchSlash}>/</span>
            <Input
              ref={worktreeBranchInputRef}
              size="sm"
              mono
              className={styles.worktreeBranchInput}
              value={createWorktreeBranchSuffixDraft}
              placeholder={t('workbench:worktrees.suffixPlaceholder')}
              aria-label={t('workbench:worktrees.suffixLabel')}
              disabled={worktreeBusy === 'create' || remoteWriteDisabled}
              onChange={(event) => setCreateWorktreeBranchSuffixDraft(event.target.value)}
            />
            <Button
              type="submit"
              size="sm"
              variant="primary"
              loading={worktreeBusy === 'create'}
              disabled={!composedWorktreeBranchName || worktreeBusy !== null || remoteWriteDisabled}
            >
              {t('common:action.confirm')}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              disabled={worktreeBusy === 'create'}
              onClick={handleCancelCreateWorktree}
            >
              {t('common:action.cancel')}
            </Button>
          </form>
        ) : (
          <Button
            size="sm"
            variant="secondary"
            icon={<PlusIcon />}
            loading={worktreeBusy === 'create'}
            disabled={!activeProjectId || worktreeBusy !== null || remoteWriteDisabled}
            onClick={handleOpenCreateWorktree}
          >
            {t('workbench:worktrees.create')}
          </Button>
        )}
      </div>
      {workspaceSwitch ? (
        <div className={styles.worktreeWorkspaceSwitch}>{workspaceSwitch}</div>
      ) : null}
    </div>
  );
}