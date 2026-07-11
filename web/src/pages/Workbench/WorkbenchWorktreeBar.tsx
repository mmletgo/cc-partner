/**
 * Workbench worktree 切换条叶子视图 —— worktree chip 列表 + 创建表单 + 移除按钮。
 *
 * Business Logic（为什么需要这个组件）:
 *   Plan 2 Task 8 把 Workbench.tsx centerPane 顶部的 worktreeBar 内部 JSX 抽到独立叶子组件，便于页面降到 ≤1200 行。
 *   外层 `<section ... hidden={automationConsoleOpen}>` 仍保留在 Workbench.tsx（workbenchAutomationView 静态契约
 *   断言该字面量出现在页面源码中），本组件只负责 section 内部的 worktree 切换 + 创建/移除表单。
 *
 * Code Logic（这个组件做什么）:
 *   - 渲染 worktree chip 列表（点击切换 active worktree，含 main/linked 文案与 status tone 点）；
 *   - 渲染 create worktree 表单（prefix select + suffix input + confirm/cancel）或“新建”按钮；
 *   - 渲染 remove worktree 按钮；
 *   - 暴露 WorkbenchWorktreeBarProps 类型，所有数据均来自 useWorkbenchWorktreeGitController + Workbench.tsx 跨域共享。
 */
import { useTranslation } from 'react-i18next';
import { Button, Input } from '@/components/primitives';
import { PlusIcon, TrashIcon } from '@/lib/icons';
import type { WorkbenchWorktree } from '@/lib/types';
import styles from './Workbench.module.css';
import {
  canRemoveWorktree,
  composeWorktreeBranchName,
  WORKTREE_BRANCH_PREFIXES,
  worktreeStatusTone,
} from './workbenchWorktrees';
import type { WorktreeBranchPrefix } from './workbenchWorktrees';
import type { WorktreeBusyKind } from './controllers/useWorkbenchWorktreeGitController';

/**
 * worktree 切换条叶子组件的输入 props。
 *
 * Business Logic: 所有数据均由 useWorkbenchWorktreeGitController + Workbench.tsx 跨域共享派生；
 * 组件本身不持有状态、不调用 workbenchApi。worktreeBranchInputRef 由页面持有并通过 forwardRef 注入到 suffix Input，
 * 因为 createWorktreeOpen 状态变化时页面需要 focus 该输入。
 */
export interface WorkbenchWorktreeBarProps {
  worktrees: WorkbenchWorktree[];
  activeWorktree: WorkbenchWorktree | null;
  activeProjectId: string | null;
  remoteWriteDisabled: boolean;
  worktreeBusy: WorktreeBusyKind | null;
  createWorktreeOpen: boolean;
  createWorktreeBranchPrefix: WorktreeBranchPrefix;
  createWorktreeBranchSuffixDraft: string;
  worktreeBranchInputRef: React.RefObject<HTMLInputElement | null>;
  setActiveWorktreeId: (id: string) => void;
  setCreateWorktreeBranchPrefix: (prefix: WorktreeBranchPrefix) => void;
  setCreateWorktreeBranchSuffixDraft: (suffix: string) => void;
  handleOpenCreateWorktree: () => void;
  handleCancelCreateWorktree: () => void;
  handleCreateWorktree: () => Promise<void>;
  handleRemoveWorktree: () => Promise<void>;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench centerPane 顶部需要稳定的 worktree 切换/创建/移除条，让用户在多 worktree 项目里快速切换上下文。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 worktreeStrip（chip 列表）+ worktreeActions（创建表单或新建按钮 + 移除按钮）；不持有状态、不调用 workbenchApi。
 */
export function WorkbenchWorktreeBar(props: WorkbenchWorktreeBarProps) {
  const { t } = useTranslation(['workbench', 'common']);
  const {
    worktrees,
    activeWorktree,
    activeProjectId,
    remoteWriteDisabled,
    worktreeBusy,
    createWorktreeOpen,
    createWorktreeBranchPrefix,
    createWorktreeBranchSuffixDraft,
    worktreeBranchInputRef,
    setActiveWorktreeId,
    setCreateWorktreeBranchPrefix,
    setCreateWorktreeBranchSuffixDraft,
    handleOpenCreateWorktree,
    handleCancelCreateWorktree,
    handleCreateWorktree,
    handleRemoveWorktree,
  } = props;

  const composedWorktreeBranchName = composeWorktreeBranchName(
    createWorktreeBranchPrefix,
    createWorktreeBranchSuffixDraft,
  );

  return (
    <>
      <div className={styles.worktreeStrip}>
        {worktrees.length === 0 ? (
          <span className={styles.worktreeEmpty}>{t('workbench:worktrees.empty')}</span>
        ) : (
          worktrees.map((worktree) => {
            const tone = worktreeStatusTone(worktree);
            const label = worktree.branch ?? worktree.name;
            return (
              <button
                key={worktree.id}
                type="button"
                className={styles.worktreeChip}
                data-active={worktree.id === activeWorktree?.id || undefined}
                data-tone={tone}
                onClick={() => setActiveWorktreeId(worktree.id)}
              >
                <span className={styles.worktreeDot} data-tone={tone} />
                <span className={styles.worktreeName}>{label}</span>
                <span className={styles.worktreeMeta}>
                  {worktree.isMain
                    ? t('workbench:worktrees.main')
                    : t('workbench:worktrees.linked')}
                </span>
              </button>
            );
          })
        )}
      </div>
      <div className={styles.worktreeActions}>
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
                  <option key={prefix} value={prefix}>
                    {prefix}
                  </option>
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
        <Button
          variant="icon"
          icon={<TrashIcon />}
          title={t('workbench:worktrees.remove')}
          aria-label={t('workbench:worktrees.remove')}
          loading={worktreeBusy === 'remove'}
          disabled={!canRemoveWorktree(activeWorktree, worktreeBusy) || remoteWriteDisabled}
          onClick={() => void handleRemoveWorktree()}
        />
      </div>
    </>
  );
}
