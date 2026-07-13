import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Input, Pill } from '@/components/primitives';
import { BrowserIcon, ExternalLinkIcon, RefreshIcon } from '@/lib/icons';
import type {
  WorkbenchBrowserDiscovery,
  WorkbenchBrowserPreview,
  WorkbenchBrowserTarget,
} from '@/lib/types';
import type { WorkbenchBrowserWorkspaceProps } from './WorkbenchBrowserWorkspace';
import {
  canApplyWorkbenchBrowserRequest,
  getWorkbenchBrowserFrameSrc,
  getWorkbenchBrowserTargetSourceLabelKey,
  WORKBENCH_BROWSER_IFRAME_SANDBOX,
  type WorkbenchBrowserRequestSnapshot,
} from './workbenchBrowserHelpers';
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
  const requestStateRef = useRef({ sequence: 0, projectId, worktreeId });
  const frameSrc = useMemo(() => getWorkbenchBrowserFrameSrc(preview, surface), [preview, surface]);
  // 同步 project/worktree 到 ref，避免 render 期写 ref 触发 react-hooks/refs。
  useEffect(() => {
    requestStateRef.current.projectId = projectId;
    requestStateRef.current.worktreeId = worktreeId;
  }, [projectId, worktreeId]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户可能快速切换 project/worktree 或连续打开候选 URL，每次异步请求都需要唯一序号防止旧结果覆盖新预览。
   *
   * Code Logic（这个函数做什么）:
   *   在当前 project 存在时递增请求序号，并捕获本次请求的 projectId/worktreeId 快照；无项目时返回 null。
   */
  const beginBrowserRequest = useCallback((): WorkbenchBrowserRequestSnapshot | null => {
    if (!projectId) return null;
    const sequence = requestStateRef.current.sequence + 1;
    const request = { sequence, projectId, worktreeId };
    requestStateRef.current.sequence = sequence;
    requestStateRef.current.projectId = projectId;
    requestStateRef.current.worktreeId = worktreeId;
    return request;
  }, [projectId, worktreeId]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   异步 discover/openTarget 返回时必须确认自己仍属于当前上下文和最新请求，旧请求不能写 state。
   *
   * Code Logic（这个函数做什么）:
   *   使用 ref 中的最新 project/worktree/sequence 与请求快照比较，返回是否允许提交结果。
   */
  const isCurrentBrowserRequest = useCallback(
    (request: WorkbenchBrowserRequestSnapshot): boolean =>
      canApplyWorkbenchBrowserRequest(requestStateRef.current, request),
    [],
  );

  const loadDiscovery = useCallback(async () => {
    const request = beginBrowserRequest();
    if (!request) return;
    setBusy(true);
    setError(null);
    try {
      const next = await transport.browser.discover(request.projectId, request.worktreeId);
      if (!isCurrentBrowserRequest(request)) return;
      setDiscovery(next);
      const selected =
        next.targets.find((target) => target.id === next.selectedTargetId) ?? next.targets[0];
      if (selected) {
        const created = await transport.browser.createPreview(
          request.projectId,
          request.worktreeId,
          selected.url,
        );
        if (!isCurrentBrowserRequest(request)) return;
        setPreview(created);
        setManualUrl(selected.displayUrl);
      }
    } catch (unknownError) {
      if (!isCurrentBrowserRequest(request)) return;
      setError(unknownError instanceof Error ? unknownError.message : String(unknownError));
    } finally {
      if (isCurrentBrowserRequest(request)) {
        setBusy(false);
      }
    }
  }, [beginBrowserRequest, isCurrentBrowserRequest, transport]);

  const openTarget = useCallback(
    async (target: WorkbenchBrowserTarget | string) => {
      const targetUrl = typeof target === 'string' ? target.trim() : target.url;
      if (!targetUrl) return;
      const request = beginBrowserRequest();
      if (!request) return;
      setBusy(true);
      setError(null);
      try {
        const created = await transport.browser.createPreview(
          request.projectId,
          request.worktreeId,
          targetUrl,
        );
        if (!isCurrentBrowserRequest(request)) return;
        setPreview(created);
        setManualUrl(typeof target === 'string' ? targetUrl : target.displayUrl);
      } catch (unknownError) {
        if (!isCurrentBrowserRequest(request)) return;
        setError(unknownError instanceof Error ? unknownError.message : String(unknownError));
      } finally {
        if (isCurrentBrowserRequest(request)) {
          setBusy(false);
        }
      }
    },
    [beginBrowserRequest, isCurrentBrowserRequest, transport],
  );

  /* eslint-disable react-hooks/set-state-in-effect -- project/worktree 切换时重置预览上下文；随后异步 loadDiscovery */
  useEffect(() => {
    setDiscovery(null);
    setPreview(null);
    setManualUrl('');
    setError(null);
    if (!projectId) {
      setBusy(false);
      return;
    }
    void loadDiscovery();
  }, [loadDiscovery, projectId, worktreeId]);
  /* eslint-enable react-hooks/set-state-in-effect */

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
              <span>{t(getWorkbenchBrowserTargetSourceLabelKey(target.source) as never)}</span>
              <span>{target.displayUrl}</span>
            </button>
          ))}
        </div>
      ) : null}
      <div className={styles.frameShell}>
        {frameSrc ? (
          <iframe
            className={styles.frame}
            sandbox={WORKBENCH_BROWSER_IFRAME_SANDBOX}
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
