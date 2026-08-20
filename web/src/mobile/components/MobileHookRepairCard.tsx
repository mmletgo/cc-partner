/**
 * 移动端 Git 钩子失败修复卡片。
 *
 * Business Logic（为什么需要这个组件）:
 *   桌面 Git 历史在 failedHook 后提供「让 AI 修复 / 重试 / 忽略」；移动端终端 FAB 与 Git 面板需要同一套出口。
 *
 * Code Logic（这个组件做什么）:
 *   展示标题、可选退出码、可展开钩子输出，以及修复/重试与忽略按钮；文案复用 worktrees.hookRepair。
 */

import { useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { formatMobileHookRepairOutput, type MobileHookRepair } from '../mobileHookRepair';
import styles from '../MobileWorkbench.module.css';

export interface MobileHookRepairCardProps {
  hookRepair: MobileHookRepair;
  busy: boolean;
  onRepair: () => void;
  onRetry: () => void;
  onDismiss: () => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端全屏和 Git 面板都要放同一块 failedHook 操作区，避免两处各写一套按钮逻辑。
 *
 * Code Logic（这个函数做什么）:
 *   有 terminalSessionId 时展示重试；否则展示「让 AI 修复」。忽略始终可用。
 */
export function MobileHookRepairCard({
  hookRepair,
  busy,
  onRepair,
  onRetry,
  onDismiss,
}: MobileHookRepairCardProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const [outputExpanded, setOutputExpanded] = useState<boolean>(false);
  const output = formatMobileHookRepairOutput(hookRepair.hookFailure);
  const title =
    hookRepair.kind === 'commit'
      ? t('workbench:worktrees.hookRepair.titleCommit')
      : t('workbench:worktrees.hookRepair.titlePush');

  return (
    <section
      className={styles.mobileHookRepairCard}
      role={hookRepair.kind === 'push' ? 'alert' : 'status'}
      aria-live={hookRepair.kind === 'push' ? 'assertive' : 'polite'}
      data-testid="mobile-hook-repair-card"
    >
      <div className={styles.mobileHookRepairHeader}>
        <strong>{title}</strong>
        {typeof hookRepair.hookFailure.exitCode === 'number' ? (
          <span>
            {t('workbench:worktrees.hookRepair.exitCode', {
              code: hookRepair.hookFailure.exitCode,
            })}
          </span>
        ) : null}
      </div>
      {hookRepair.terminalSessionId ? (
        <p className={styles.panelState}>{t('workbench:worktrees.hookRepair.terminalHint')}</p>
      ) : null}
      <button
        type="button"
        className={styles.secondaryButton}
        aria-expanded={outputExpanded}
        onClick={() => setOutputExpanded((open) => !open)}
      >
        {outputExpanded
          ? t('workbench:worktrees.hookRepair.hideOutput')
          : t('workbench:worktrees.hookRepair.showOutput')}
      </button>
      {outputExpanded ? (
        <pre className={styles.mobileHookRepairOutput}>
          {output || t('workbench:worktrees.hookRepair.noOutput')}
        </pre>
      ) : null}
      <div className={styles.mobileHookRepairActions}>
        {hookRepair.terminalSessionId ? (
          <button
            type="button"
            className={styles.mobileTerminalPrimaryButton}
            disabled={busy}
            onClick={onRetry}
          >
            {hookRepair.kind === 'commit'
              ? t('workbench:worktrees.hookRepair.retryCommit')
              : t('workbench:worktrees.hookRepair.retryPush')}
          </button>
        ) : (
          <button
            type="button"
            className={styles.mobileTerminalPrimaryButton}
            disabled={busy}
            aria-busy={busy || undefined}
            onClick={onRepair}
          >
            {busy
              ? t('workbench:worktrees.hookRepair.runButtonBusy')
              : t('workbench:worktrees.hookRepair.runButton')}
          </button>
        )}
        <button type="button" className={styles.secondaryButton} onClick={onDismiss}>
          {t('workbench:worktrees.hookRepair.dismissButton')}
        </button>
      </div>
    </section>
  );
}
