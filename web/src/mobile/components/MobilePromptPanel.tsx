import { useCallback, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import { useLanguage } from '@/hooks/useLanguage';
import type { WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import styles from '../MobileWorkbench.module.css';

export interface MobilePromptPanelProps {
  worktree: WorkbenchWorktree | null;
  session: WorkbenchSession | null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Prompt 优化失败时移动端需要显示后端返回的可读错误，并兼容非 Error 抛出值。
 *
 * Code Logic（这个函数做什么）:
 *   优先读取 Error.message；其它 unknown 值转为字符串。
 */
function getErrorMessage(reason: unknown): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  return String(reason);
}

/**
 * MobilePromptPanel（移动端 Prompt 优化面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端 Workbench 需要把用户输入的原始 Prompt 交给本机 Claude Code CLI 优化，并写入当前终端窗口。
 *
 * Code Logic（这个组件做什么）:
 *   管理原始 Prompt 状态，调用 HTTP prompt transport；优化结果语言跟随当前界面语种；
 *   成功后清空输入，只显示短状态，不本地拼接终端输出。
 */
export function MobilePromptPanel({ worktree, session }: MobilePromptPanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const { language } = useLanguage();
  const [rawPrompt, setRawPrompt] = useState<string>('');
  const [submitting, setSubmitting] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const trimmedPrompt = rawPrompt.trim();
  const canSubmit = Boolean(worktree && session && trimmedPrompt && !submitting);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击“写入当前终端”时，需要把优化请求交给后端，并让后端流式写入已有 tmux-backed session。
   *
   * Code Logic（这个函数做什么）:
   *   调用 prompt.streamToTerminal，传入 worktree.path 作为 workingDirectory 和当前 sessionId；成功后清空输入和错误。
   */
  const handleSubmit = useCallback(async (): Promise<void> => {
    if (!worktree || !session || !trimmedPrompt) return;
    setSubmitting(true);
    setError(null);
    setStatus(null);
    try {
      await httpWorkbenchTransport.prompt.streamToTerminal(trimmedPrompt, {
        workingDirectory: worktree.path,
        targetLanguage: language,
        sessionId: session.id,
      });
      setRawPrompt('');
      setStatus(t('workbench:mobile.promptPanel.sent'));
    } catch (reason) {
      setError(`${t('workbench:mobile.promptPanel.error')}: ${getErrorMessage(reason)}`);
    } finally {
      setSubmitting(false);
    }
  }, [language, session, t, trimmedPrompt, worktree]);

  return (
    <section className={styles.panel} aria-labelledby="mobile-prompt-panel-title">
      <div className={styles.panelHeader}>
        <h1 id="mobile-prompt-panel-title">{t('workbench:mobile.promptPanel.title')}</h1>
      </div>

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

      <div className={styles.mobileForm}>
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
    </section>
  );
}
