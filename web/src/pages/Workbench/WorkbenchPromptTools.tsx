/**
 * Workbench 终端工具栏的 Prompt 工具按钮组
 *
 * Business Logic（为什么需要这个组件）:
 *   「Prompt 优化」与「收藏快捷输入」是终端工具栏里两个并列的 Prompt 工具入口，
 *   抽成独立组件让两者紧邻渲染，同时避免 Workbench.tsx 因新增按钮 JSX 超出 1200 行上限。
 *
 * Code Logic（这个组件做什么）:
 *   渲染两个 Button（图标 + 响应式文字），共享 hasActiveSession/remoteWriteDisabled 启用态，
 *   各自用 data-active 高亮当前打开的浮层。无 API、无副作用，纯受控视图。
 */
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/primitives';
import { EditIcon, StarIcon } from '@/lib/icons';
import styles from './Workbench.module.css';

export interface WorkbenchPromptToolsProps {
  /** 是否存在可写入的活跃终端会话 */
  hasActiveSession: boolean;
  /** 远端项目离线等导致的写禁用 */
  remoteWriteDisabled: boolean;
  /** Prompt 优化浮层是否打开（高亮 + 启用态） */
  promptPanelOpen: boolean;
  onTogglePromptOptimizer: () => void;
  /** 收藏快捷输入面板是否打开 */
  favoriteOpen: boolean;
  onToggleFavorite: () => void;
}

/**
 * 渲染 Prompt 优化 + 收藏快捷输入两个并列工具栏按钮。
 *
 * Business Logic（为什么需要这个函数）:
 *   Workbench.tsx 在工具栏 actions slot 调用本组件，保持两个 Prompt 入口紧邻且样式一致。
 *
 * Code Logic（这个函数做什么）:
 *   两个 Button 复用 terminalActionButton 样式与响应式 label；启用条件对齐既有 Prompt 优化按钮。
 */
export function WorkbenchPromptTools({
  hasActiveSession,
  remoteWriteDisabled,
  promptPanelOpen,
  onTogglePromptOptimizer,
  favoriteOpen,
  onToggleFavorite,
}: WorkbenchPromptToolsProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  return (
    <>
      <Button
        className={styles.terminalActionButton}
        variant="secondary"
        size="sm"
        icon={<EditIcon />}
        title={t('workbench:promptOptimizer.open')}
        aria-label={t('workbench:promptOptimizer.open')}
        data-workbench-responsive-action="true"
        data-active={promptPanelOpen || undefined}
        disabled={!hasActiveSession || (remoteWriteDisabled && !promptPanelOpen)}
        onClick={onTogglePromptOptimizer}
      >
        <span data-workbench-responsive-label="true">{t('workbench:promptOptimizer.open')}</span>
      </Button>
      <Button
        className={styles.terminalActionButton}
        variant="secondary"
        size="sm"
        icon={<StarIcon />}
        title={t('workbench:favoriteQuickInput.open')}
        aria-label={t('workbench:favoriteQuickInput.open')}
        data-workbench-responsive-action="true"
        data-active={favoriteOpen || undefined}
        disabled={!hasActiveSession || (remoteWriteDisabled && !favoriteOpen)}
        onClick={onToggleFavorite}
      >
        <span data-workbench-responsive-label="true">{t('workbench:favoriteQuickInput.open')}</span>
      </Button>
    </>
  );
}
