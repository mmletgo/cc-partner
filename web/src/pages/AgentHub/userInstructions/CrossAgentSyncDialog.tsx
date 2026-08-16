import { useCallback, useEffect, useMemo, useRef, useState, type JSX } from 'react';
import { Button, Dialog, Pill, StatusMessage } from '@/components/primitives';
import { agentHubApi } from '@/api/agentHub';
import { allHubTargets } from '@/lib/agentCatalog';
import type { AgentTarget } from '@/lib/types/agentHub';
import type { TFunction } from 'i18next';
import {
  adaptModeTone,
  parseCrossAgentPreview,
  type CrossAgentPreviewReport,
} from '../crossAgent/crossAgentPresentation';
import styles from '../AgentHub.module.css';

export interface CrossAgentSyncDialogProps {
  t: TFunction<['agentHub', 'common']>;
  open: boolean;
  sourceMarkdown: string;
  onClose: () => void;
}

const ALL_TARGETS: AgentTarget[] = allHubTargets();

/** 响应必须与本次源和目标集合完全一致，且永远不可 Apply。 */
function previewMatchesRequest(
  preview: CrossAgentPreviewReport,
  source: AgentTarget,
  destinations: AgentTarget[],
): boolean {
  if (preview.source !== source || preview.destinations.length !== destinations.length) {
    return false;
  }
  const expected = new Set(destinations);
  return preview.destinations.every(
    (row) => expected.has(row.destination) && row.destination !== source && !row.canApply,
  );
}

/**
 * Business Logic（为什么需要）:
 *   旧入口仍可能被复用，因此也必须遵守“本机用户级选择性预览、零写入”的当前能力边界。
 *
 * Code Logic（做什么）:
 *   复用共享严格 decoder；输入变化递增 sequence 丢弃旧响应；生产 DOM 不渲染 Apply。
 */
export function CrossAgentSyncDialog(props: CrossAgentSyncDialogProps): JSX.Element {
  const { t, open, sourceMarkdown, onClose } = props;
  const [source, setSource] = useState<AgentTarget>('claude');
  const [destinations, setDestinations] = useState<AgentTarget[]>(['codex']);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<CrossAgentPreviewReport | null>(null);
  const requestSeqRef = useRef(0);

  const markdown = useMemo(() => sourceMarkdown.trim(), [sourceMarkdown]);
  const canPreview =
    markdown.length > 0 && destinations.length > 0 && !destinations.includes(source);

  useEffect(() => {
    requestSeqRef.current += 1;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- a reopened preview session must atomically discard prior async UI.
    setBusy(false);
    setError(null);
    setPreview(null);
  }, [open, sourceMarkdown]);

  const invalidatePreview = useCallback(() => {
    requestSeqRef.current += 1;
    setBusy(false);
    setPreview(null);
    setError(null);
  }, []);

  const toggleDestination = useCallback(
    (target: AgentTarget) => {
      if (target === source) return;
      invalidatePreview();
      setDestinations((current) =>
        current.includes(target)
          ? current.filter((destination) => destination !== target)
          : [...current, target],
      );
    },
    [invalidatePreview, source],
  );

  const runPreview = useCallback(async () => {
    if (!canPreview) return;
    const requestedSource = source;
    const requestedDestinations = [...destinations];
    const requestedMarkdown = markdown;
    const seq = ++requestSeqRef.current;
    setBusy(true);
    setError(null);
    setPreview(null);
    try {
      const raw = await agentHubApi.previewCrossAgentInstruction({
        source: requestedSource,
        destinations: requestedDestinations,
        sourceMarkdown: requestedMarkdown,
        scope: 'user',
        destinationPaths: {},
      });
      if (seq !== requestSeqRef.current) return;
      const parsed = parseCrossAgentPreview(raw);
      if (!parsed || !previewMatchesRequest(parsed, requestedSource, requestedDestinations)) {
        setError(t('agentHub:crossAgent.errors.invalidPreview'));
        return;
      }
      setPreview(parsed);
    } catch (reason) {
      if (seq !== requestSeqRef.current) return;
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      if (seq === requestSeqRef.current) setBusy(false);
    }
  }, [canPreview, destinations, markdown, source, t]);

  const handleClose = useCallback(() => {
    if (busy) return;
    invalidatePreview();
    onClose();
  }, [busy, invalidatePreview, onClose]);

  return (
    <Dialog
      open={open}
      titleId="cross-agent-sync-title"
      onClose={handleClose}
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
      className={styles.userPreviewSurface}
    >
      <div className={styles.userDialogBody} data-testid="cross-agent-sync-dialog">
        <header className={styles.userDialogHeader}>
          <h2 id="cross-agent-sync-title" className={styles.userDialogTitle}>
            {t('agentHub:crossAgent.title')}
          </h2>
          <p className={styles.userSectionDescription}>
            {t('agentHub:crossAgent.description')}
          </p>
        </header>

        <StatusMessage tone="info" live="off" data-testid="cross-agent-preview-only">
          {t('agentHub:crossAgent.previewOnly')}
        </StatusMessage>

        {error ? (
          <StatusMessage tone="danger" data-testid="cross-agent-error">
            {error}
          </StatusMessage>
        ) : null}

        <section className={styles.userPlanChanges} aria-label={t('agentHub:crossAgent.selectAria')}>
          <label className={styles.filterField}>
            <span>{t('agentHub:crossAgent.sourceLabel')}</span>
            <select
              value={source}
              disabled={busy}
              data-testid="cross-agent-source"
              onChange={(event) => {
                const next = event.currentTarget.value as AgentTarget;
                invalidatePreview();
                setSource(next);
                setDestinations((current) => current.filter((target) => target !== next));
              }}
            >
              {ALL_TARGETS.map((target) => (
                <option key={target} value={target}>
                  {t(`agentHub:targets.${target}`)}
                </option>
              ))}
            </select>
          </label>

          <fieldset className={styles.userTargetGrid} data-testid="cross-agent-destinations">
            <legend>{t('agentHub:crossAgent.destinationsLabel')}</legend>
            {ALL_TARGETS.filter((target) => target !== source).map((target) => (
              <label key={target} className={styles.filterField}>
                <input
                  type="checkbox"
                  checked={destinations.includes(target)}
                  disabled={busy}
                  data-testid={`cross-agent-dest-${target}`}
                  onChange={() => toggleDestination(target)}
                />
                <span>{t(`agentHub:targets.${target}`)}</span>
              </label>
            ))}
          </fieldset>

          <p className={styles.hint} data-testid="cross-agent-source-length">
            {t('agentHub:crossAgent.contentChars', { count: markdown.length })}
          </p>
        </section>

        {preview ? (
          <section
            className={styles.userPlanChanges}
            data-testid="cross-agent-preview-result"
            aria-label={t('agentHub:crossAgent.previewAria')}
          >
            {preview.destinations.map((row) => (
              <article
                key={row.destination}
                className={styles.userPlanChange}
                data-testid={`cross-agent-preview-${row.destination}`}
              >
                <div className={styles.userPlanHeader}>
                  <div>
                    <h3>{t(`agentHub:targets.${row.destination}`)}</h3>
                    <code className={styles.userPath}>{row.path || '—'}</code>
                  </div>
                  <Pill tone={adaptModeTone(row.mode)}>
                    {t(`agentHub:crossAgent.modes.${row.mode}`, {
                      defaultValue: row.mode,
                    })}
                  </Pill>
                </div>
                {row.partialBlockers.length > 0 ? (
                  <ul className={styles.userWarningList}>
                    {row.partialBlockers.map((blocker) => (
                      <li key={blocker}>{blocker}</li>
                    ))}
                  </ul>
                ) : null}
                {row.unifiedDiff == null ? (
                  <StatusMessage tone="warn" live="off">
                    {t('agentHub:crossAgent.diffUnavailable')}
                  </StatusMessage>
                ) : row.unifiedDiff.length === 0 ? (
                  <StatusMessage tone="success" live="off">
                    {t('agentHub:crossAgent.noChanges')}
                  </StatusMessage>
                ) : (
                  <pre className={styles.blockBody}>{row.unifiedDiff}</pre>
                )}
              </article>
            ))}
          </section>
        ) : null}

        <footer className={styles.dialogActions}>
          <Button
            variant="secondary"
            size="sm"
            disabled={busy}
            onClick={handleClose}
            data-testid="cross-agent-close"
          >
            {t('common:action.cancel')}
          </Button>
          <Button
            variant="primary"
            size="sm"
            loading={busy}
            disabled={!canPreview || busy}
            onClick={() => void runPreview()}
            data-testid="cross-agent-preview"
          >
            {t('agentHub:crossAgent.preview')}
          </Button>
        </footer>
      </div>
    </Dialog>
  );
}
