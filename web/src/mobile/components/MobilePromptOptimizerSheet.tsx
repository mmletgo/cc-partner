/**
 * 移动端「Prompt 优化」终端内浮层（bottom sheet）。
 *
 * Business Logic（为什么需要这个组件）:
 *   用户在移动端终端工作时，经终端右下角悬浮按钮唤起 Prompt 优化输入，把原始 Prompt 交给本机
 *   Claude Code CLI 优化并流式写入当前终端，无需离开终端视图。与 MobilePromptPanel（nav panel）
 *   等价逻辑，但以 Dialog bottom sheet 形式悬浮在终端上下文，符合「终端内浮层」的交互预期。
 *
 * Code Logic（这个组件做什么）:
 *   - 共享 Dialog 原语渲染 bottom sheet（portal / Escape / backdrop / focus trap），禁止手写 modal。
 *   - 自管原始 Prompt / 目标语种 / 提交态 / 错误 / 状态，调用 httpWorkbenchTransport.prompt.streamToTerminal。
 *   - 提交成功清空输入并提示已发送；失败展示可读错误。颜色/间距走 tokens，文案走 t('workbench:mobile.promptPanel.*')。
 *   - 所有 hooks 在 return 之前；open=false 时由 Dialog 返回 null。
 */
import { useCallback, useId, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Dialog } from '@/components/primitives';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import type {
  PromptOptimizerFillLanguage,
  WorkbenchSession,
  WorkbenchWorktree,
} from '@/lib/types';
import styles from '../MobileWorkbench.module.css';

export interface MobilePromptOptimizerSheetProps {
  open: boolean;
  onClose: () => void;
  worktree: WorkbenchWorktree | null;
  session: WorkbenchSession | null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Prompt 优化失败时需要展示后端返回的可读错误，并兼容非 Error 抛出值。
 *
 * Code Logic（这个函数做什么）:
 *   优先读取 Error.message；其它 unknown 值转为字符串。
 */
function getErrorMessage(reason: unknown): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  return String(reason);
}

export function MobilePromptOptimizerSheet({
  open,
  onClose,
  worktree,
  session,
}: MobilePromptOptimizerSheetProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const titleId = useId();
  const [rawPrompt, setRawPrompt] = useState<string>('');
  const [targetLanguage, setTargetLanguage] = useState<PromptOptimizerFillLanguage>('zh');
  const [submitting, setSubmitting] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const trimmedPrompt = rawPrompt.trim();
  const canSubmit = Boolean(worktree && session && trimmedPrompt && !submitting);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击写入终端时，把优化请求交给后端，由后端流式写入当前 tmux-backed session。
   *
   * Code Logic（这个函数做什么）:
   *   调用 prompt.streamToTerminal，传 worktree.path 作 workingDirectory + 当前 sessionId；成功后清空输入。
   */
  const handleSubmit = useCallback(async (): Promise<void> => {
    if (!worktree || !session || !trimmedPrompt) return;
    setSubmitting(true);
    setError(null);
    setStatus(null);
    try {
      await httpWorkbenchTransport.prompt.streamToTerminal(trimmedPrompt, {
        workingDirectory: worktree.path,
        targetLanguage,
        sessionId: session.id,
      });
      setRawPrompt('');
      setStatus(t('workbench:mobile.promptPanel.sent'));
    } catch (reason) {
      setError(`${t('workbench:mobile.promptPanel.error')}: ${getErrorMessage(reason)}`);
    } finally {
      setSubmitting(false);
    }
  }, [session, t, targetLanguage, trimmedPrompt, worktree]);

  return (
    <Dialog
      open={open}
      onClose={onClose}
      className={styles.favoriteSheet}
      titleId={titleId}
      closeOnEscape={!submitting}
      closeOnBackdrop={!submitting}
    >
      <header className={styles.favoriteHeader}>
        <h2 id={titleId} className={styles.favoriteTitle}>{t('workbench:mobile.promptPanel.title')}</h2>
      </header>
      <div className={styles.mobileForm}>
        {!worktree ? (
          <p className={styles.panelState}>{t('workbench:mobile.promptPanel.noWorktree')}</p>
        ) : null}
        {!session ? (
          <p className={styles.panelState}>{t('workbench:mobile.promptPanel.noSession')}</p>
        ) : null}
        {error ? (
          <p className={styles.panelError}>
            <span>{t('workbench:mobile.projectPanel.error')}</span>
            <span>{error}</span>
          </p>
        ) : null}
        {status ? <p className={styles.panelState}>{status}</p> : null}

        <label className={styles.mobileField}>
          <span>{t('workbench:mobile.promptPanel.promptLabel')}</span>
          <textarea
            className={styles.mobileTextarea}
            value={rawPrompt}
            disabled={submitting}
            placeholder={t('workbench:mobile.promptPanel.promptPlaceholder')}
            onChange={(event) => {
              setRawPrompt(event.target.value);
              setStatus(null);
            }}
          />
        </label>

        <label className={styles.mobileField}>
          <span>{t('workbench:mobile.promptPanel.languageLabel')}</span>
          <select
            className={styles.mobileSelect}
            value={targetLanguage}
            disabled={submitting}
            onChange={(event) => {
              setTargetLanguage(event.target.value as PromptOptimizerFillLanguage);
            }}
          >
            <option value="zh">{t('workbench:mobile.promptPanel.languages.zh')}</option>
            <option value="en">{t('workbench:mobile.promptPanel.languages.en')}</option>
          </select>
        </label>

        <button
          type="button"
          className={styles.mobileTerminalPrimaryButton}
          disabled={!canSubmit}
          onClick={() => void handleSubmit()}
        >
          {submitting
            ? t('workbench:mobile.promptPanel.sending')
            : t('workbench:mobile.promptPanel.send')}
        </button>
      </div>
    </Dialog>
  );
}
