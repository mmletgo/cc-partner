import { useCallback, useEffect, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Pill } from '@/components/primitives';
import type { BrowserVerificationRun } from '@/lib/types';
import type { WorkbenchTransport } from '@/api/workbenchTransport';
import {
  buildDefaultVerificationStart,
  isBrowserVerificationTerminal,
  screenshotDataUrl,
  summarizeVerification,
} from './workbenchBrowserVerification';
import styles from './WorkbenchBrowserVerificationPanel.module.css';

export interface WorkbenchBrowserVerificationPanelProps {
  previewId: string | null;
  transport: WorkbenchTransport;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   用户需要一键验证当前 Browser preview，默认自动 smoke，不写脚本/选元素。
 *
 * Code Logic（这个组件做什么）:
 *   调用 transport.browser.startVerification，轮询 get，展示截图与 console/assertion 摘要。
 */
export function WorkbenchBrowserVerificationPanel({
  previewId,
  transport,
}: WorkbenchBrowserVerificationPanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [run, setRun] = useState<BrowserVerificationRun | null>(null);
  const [shotSrc, setShotSrc] = useState<string | null>(null);

  // hooks 必须在 early return 之前
  const start = useCallback(async () => {
    if (!previewId || !transport.browser.startVerification) {
      setError(t('workbench:browserVerification.unavailable'));
      return;
    }
    setBusy(true);
    setError(null);
    setShotSrc(null);
    try {
      const requestId =
        typeof crypto !== 'undefined' && 'randomUUID' in crypto
          ? crypto.randomUUID()
          : `req-${Date.now()}`;
      const payload = buildDefaultVerificationStart(previewId, requestId);
      const started = await transport.browser.startVerification(
        payload.previewId,
        payload.requestId,
      );
      setRun(started);
      let current = started;
      for (let i = 0; i < 60; i += 1) {
        if (isBrowserVerificationTerminal(current.session.state)) break;
        await new Promise((r) => setTimeout(r, 250));
        if (!transport.browser.getVerification) break;
        current = await transport.browser.getVerification(current.session.id);
        setRun(current);
      }
      const summary = summarizeVerification(current);
      if (
        summary.screenshotId &&
        transport.browser.getVerificationArtifact &&
        current.session.state === 'succeeded'
      ) {
        const art = await transport.browser.getVerificationArtifact(
          current.session.id,
          summary.screenshotId,
        );
        setShotSrc(screenshotDataUrl(art.base64));
      }
    } catch (unknownError) {
      setError(unknownError instanceof Error ? unknownError.message : String(unknownError));
    } finally {
      setBusy(false);
    }
  }, [previewId, t, transport.browser]);

  useEffect(() => {
    setRun(null);
    setShotSrc(null);
    setError(null);
  }, [previewId]);

  const summary = summarizeVerification(run);
  const canStart = Boolean(previewId && transport.browser.startVerification);

  return (
    <div className={styles.root} data-testid="browser-verification-panel">
      <div className={styles.row}>
        <Button
          variant="primary"
          size="sm"
          loading={busy}
          disabled={!canStart || busy}
          onClick={() => void start()}
        >
          {t('workbench:browserVerification.verifyCurrent')}
        </Button>
        {run ? (
          <Pill tone={run.session.state === 'succeeded' ? 'success' : 'neutral'}>
            {t(`workbench:browserVerification.status.${run.session.state}` as never)}
          </Pill>
        ) : null}
      </div>
      {error ? (
        <div className={styles.error} role="alert">
          {error}
        </div>
      ) : null}
      {run?.evidence ? (
        <div className={styles.summary}>
          <div>
            {t('workbench:browserVerification.urlPath')}: {summary.urlPath ?? '—'}
          </div>
          <div>
            {t('workbench:browserVerification.consoleErrors')}: {summary.consoleErrors}
          </div>
          <div>
            {t('workbench:browserVerification.assertionFailed')}: {summary.assertionFailed}
          </div>
          {shotSrc ? (
            <img
              className={styles.shot}
              src={shotSrc}
              alt={t('workbench:browserVerification.screenshotAlt')}
            />
          ) : null}
        </div>
      ) : null}
      {/* 明确不提供脚本/selector 输入 */}
    </div>
  );
}
