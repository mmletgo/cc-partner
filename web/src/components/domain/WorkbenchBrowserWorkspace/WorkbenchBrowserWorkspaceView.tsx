import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Input, Pill } from '@/components/primitives';
import { BrowserIcon, ExternalLinkIcon, RefreshIcon } from '@/lib/icons';
import type {
  WorkbenchBrowserDiscovery,
  WorkbenchBrowserPreview,
  WorkbenchBrowserTarget,
} from '@/lib/types';
import {
  getWorkbenchBrowserFrameSrc,
  type WorkbenchBrowserWorkspaceProps,
} from './WorkbenchBrowserWorkspace';
import styles from './WorkbenchBrowserWorkspace.module.css';

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench 浏览器预览需要把自动发现候选、手动 URL 输入、preview session 和 iframe 展示组合成一个可复用工作区。
 *
 * Code Logic（这个组件做什么）:
 *   根据 project/worktree 调用 transport.browser.discover，展示候选目标，创建 preview session 后用 iframe 加载代理 URL。
 */
export function WorkbenchBrowserWorkspaceView({
  surface,
  transport,
  project,
  worktree,
  onReturnToTerminal,
}: WorkbenchBrowserWorkspaceProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const [discovery, setDiscovery] = useState<WorkbenchBrowserDiscovery | null>(null);
  const [preview, setPreview] = useState<WorkbenchBrowserPreview | null>(null);
  const [manualUrl, setManualUrl] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const projectId = project?.id ?? null;
  const worktreeId = worktree?.id ?? null;
  const frameSrc = useMemo(() => getWorkbenchBrowserFrameSrc(preview, surface), [preview, surface]);

  const loadDiscovery = useCallback(async () => {
    if (!projectId) return;
    setBusy(true);
    setError(null);
    try {
      const next = await transport.browser.discover(projectId, worktreeId);
      setDiscovery(next);
      const selected =
        next.targets.find((target) => target.id === next.selectedTargetId) ?? next.targets[0];
      if (selected) {
        const created = await transport.browser.createPreview(projectId, worktreeId, selected.url);
        setPreview(created);
        setManualUrl(selected.displayUrl);
      }
    } catch (unknownError) {
      setError(unknownError instanceof Error ? unknownError.message : String(unknownError));
    } finally {
      setBusy(false);
    }
  }, [projectId, transport, worktreeId]);

  const openTarget = useCallback(
    async (target: WorkbenchBrowserTarget | string) => {
      if (!projectId) return;
      const targetUrl = typeof target === 'string' ? target.trim() : target.url;
      if (!targetUrl) return;
      setBusy(true);
      setError(null);
      try {
        const created = await transport.browser.createPreview(projectId, worktreeId, targetUrl);
        setPreview(created);
        setManualUrl(typeof target === 'string' ? targetUrl : target.displayUrl);
      } catch (unknownError) {
        setError(unknownError instanceof Error ? unknownError.message : String(unknownError));
      } finally {
        setBusy(false);
      }
    },
    [projectId, transport, worktreeId],
  );

  useEffect(() => {
    setDiscovery(null);
    setPreview(null);
    setManualUrl('');
    setError(null);
    if (projectId) void loadDiscovery();
  }, [loadDiscovery, projectId, worktreeId]);

  return (
    <section className={styles.workspace} aria-label={t('workbench:browserPreview.title')}>
      <header className={styles.toolbar}>
        <div className={styles.heading}>
          <BrowserIcon />
          <span>{t('workbench:browserPreview.title')}</span>
          {preview ? <Pill tone="success">{t('workbench:browserPreview.connected')}</Pill> : null}
        </div>
        <div className={styles.actions}>
          {onReturnToTerminal ? (
            <Button variant="secondary" size="sm" onClick={onReturnToTerminal}>
              {t('workbench:fileWorkspace.returnTerminal')}
            </Button>
          ) : null}
          <Button
            variant="secondary"
            size="sm"
            icon={<RefreshIcon />}
            loading={busy}
            disabled={!projectId}
            onClick={() => void loadDiscovery()}
          >
            {t('workbench:browserPreview.refresh')}
          </Button>
        </div>
      </header>
      <div className={styles.targetBar}>
        <Input
          className={styles.urlInput}
          value={manualUrl}
          placeholder={t('workbench:browserPreview.urlPlaceholder')}
          mono
          onChange={(event) => setManualUrl(event.target.value)}
          onKeyDown={(event) => {
            if (event.key !== 'Enter') return;
            event.preventDefault();
            void openTarget(manualUrl);
          }}
        />
        <Button
          variant="primary"
          size="sm"
          icon={<ExternalLinkIcon />}
          disabled={!projectId || !manualUrl.trim() || busy}
          onClick={() => void openTarget(manualUrl)}
        >
          {t('workbench:browserPreview.open')}
        </Button>
      </div>
      {error ? <div className={styles.error}>{error}</div> : null}
      {discovery?.targets.length ? (
        <div className={styles.targets}>
          {discovery.targets.map((target) => (
            <button
              key={target.id}
              type="button"
              className={styles.targetChip}
              data-active={preview?.targetUrl === target.url || undefined}
              onClick={() => void openTarget(target)}
            >
              <span>{target.label}</span>
              <span>{target.displayUrl}</span>
            </button>
          ))}
        </div>
      ) : null}
      <div className={styles.frameShell}>
        {frameSrc ? (
          <iframe
            className={styles.frame}
            src={frameSrc}
            title={t('workbench:browserPreview.frameTitle')}
          />
        ) : (
          <div className={styles.empty}>{t('workbench:browserPreview.empty')}</div>
        )}
      </div>
    </section>
  );
}
