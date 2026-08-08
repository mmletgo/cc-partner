import { useCallback, useMemo, useState, type JSX } from 'react';
import { Button, Dialog, Pill, StatusMessage } from '@/components/primitives';
import { agentHubApi } from '@/api/agentHub';
import type { AgentTarget } from '@/lib/types/agentHub';
import type { TFunction } from 'i18next';
import styles from '../AgentHub.module.css';

export interface CrossAgentSyncDialogProps {
  t: TFunction<['agentHub', 'common']>;
  open: boolean;
  sourceMarkdown: string;
  onClose: () => void;
}

interface CrossAgentTargetPreviewRow {
  destination: AgentTarget;
  mode: string;
  path: string;
  renderedHash?: string | null;
  unifiedDiff?: string | null;
  partialBlockers: string[];
  canApply: boolean;
}

interface CrossAgentPreviewReport {
  source: AgentTarget;
  kind: string;
  destinations: CrossAgentTargetPreviewRow[];
  needsAdaptation: boolean;
}

interface CrossAgentApplyResult {
  destination: AgentTarget;
  status: string;
  path: string;
  errorCode?: string | null;
}

const ALL_TARGETS: AgentTarget[] = ['claude', 'codex', 'opencode'];

function isAgentTarget(value: unknown): value is AgentTarget {
  return value === 'claude' || value === 'codex' || value === 'opencode';
}

function parsePreview(raw: unknown): CrossAgentPreviewReport | null {
  if (!raw || typeof raw !== 'object') return null;
  const obj = raw as Record<string, unknown>;
  if (!isAgentTarget(obj.source) || !Array.isArray(obj.destinations)) return null;
  const destinations: CrossAgentTargetPreviewRow[] = [];
  for (const row of obj.destinations) {
    if (!row || typeof row !== 'object') continue;
    const r = row as Record<string, unknown>;
    if (!isAgentTarget(r.destination) || typeof r.path !== 'string') continue;
    destinations.push({
      destination: r.destination,
      mode: typeof r.mode === 'string' ? r.mode : 'residual',
      path: r.path,
      renderedHash: typeof r.renderedHash === 'string' ? r.renderedHash : null,
      unifiedDiff: typeof r.unifiedDiff === 'string' ? r.unifiedDiff : null,
      partialBlockers: Array.isArray(r.partialBlockers)
        ? r.partialBlockers.filter((b): b is string => typeof b === 'string')
        : [],
      canApply: Boolean(r.canApply),
    });
  }
  return {
    source: obj.source,
    kind: typeof obj.kind === 'string' ? obj.kind : 'instruction',
    destinations,
    needsAdaptation: Boolean(obj.needsAdaptation),
  };
}

function parseApplyResults(raw: unknown): CrossAgentApplyResult[] {
  if (!Array.isArray(raw)) return [];
  const out: CrossAgentApplyResult[] = [];
  for (const row of raw) {
    if (!row || typeof row !== 'object') continue;
    const r = row as Record<string, unknown>;
    if (!isAgentTarget(r.destination) || typeof r.path !== 'string') continue;
    out.push({
      destination: r.destination,
      status: typeof r.status === 'string' ? r.status : 'failed',
      path: r.path,
      errorCode: typeof r.errorCode === 'string' ? r.errorCode : null,
    });
  }
  return out;
}

/**
 * Business Logic（为什么需要）:
 *   阶段三要求用户在同机手动选择源/目标 Agent，预览适配后确认一次性写入，禁止后台跨 target。
 *
 * Code Logic（做什么）:
 *   选择 source + destinations → previewCrossAgentInstruction → 展示 mode/blockers →
 *   applyCrossAgentInstruction；views 不承载 transport 细节。
 */
export function CrossAgentSyncDialog(props: CrossAgentSyncDialogProps): JSX.Element {
  const { t, open, sourceMarkdown, onClose } = props;
  const [source, setSource] = useState<AgentTarget>('claude');
  const [destinations, setDestinations] = useState<AgentTarget[]>(['codex']);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<CrossAgentPreviewReport | null>(null);
  const [applyResults, setApplyResults] = useState<CrossAgentApplyResult[] | null>(null);

  const markdown = useMemo(() => sourceMarkdown.trim(), [sourceMarkdown]);
  const canPreview = markdown.length > 0 && destinations.length > 0 && !destinations.includes(source);

  const toggleDestination = useCallback(
    (target: AgentTarget) => {
      if (target === source) return;
      setDestinations((prev) =>
        prev.includes(target) ? prev.filter((d) => d !== target) : [...prev, target],
      );
      setPreview(null);
      setApplyResults(null);
    },
    [source],
  );

  const runPreview = useCallback(async () => {
    if (!canPreview) return;
    setBusy(true);
    setError(null);
    setApplyResults(null);
    try {
      const raw = await agentHubApi.previewCrossAgentInstruction({
        source,
        destinations,
        sourceMarkdown: markdown,
        destinationPaths: {},
      });
      const parsed = parsePreview(raw);
      if (!parsed) {
        setError(t('agentHub:crossAgent.errors.invalidPreview'));
        setPreview(null);
        return;
      }
      setPreview(parsed);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      setPreview(null);
    } finally {
      setBusy(false);
    }
  }, [canPreview, destinations, markdown, source, t]);

  const runApply = useCallback(async () => {
    if (!preview) return;
    const applicable = preview.destinations.filter((d) => d.canApply).map((d) => d.destination);
    if (applicable.length === 0) return;
    setBusy(true);
    setError(null);
    try {
      const raw = await agentHubApi.applyCrossAgentInstruction({
        source,
        destinations: applicable,
        sourceMarkdown: markdown,
        destinationPaths: {},
        clientRequestId: `cross-agent-${Date.now()}`,
      });
      setApplyResults(parseApplyResults(raw));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  }, [markdown, preview, source]);

  const handleClose = useCallback(() => {
    if (busy) return;
    setError(null);
    setPreview(null);
    setApplyResults(null);
    onClose();
  }, [busy, onClose]);

  const applicableCount = preview?.destinations.filter((d) => d.canApply).length ?? 0;

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
          <p className={styles.userSectionDescription}>{t('agentHub:crossAgent.description')}</p>
        </header>

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
                setSource(next);
                setDestinations((prev) => prev.filter((d) => d !== next));
                setPreview(null);
                setApplyResults(null);
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
            {preview.needsAdaptation ? (
              <StatusMessage tone="warn" live="off">
                {t('agentHub:crossAgent.needsAdaptation')}
              </StatusMessage>
            ) : null}
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
                  <Pill tone={row.canApply ? 'success' : 'warn'}>
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
                {!row.canApply ? (
                  <StatusMessage tone="warn" live="off">
                    {t('agentHub:crossAgent.cannotApply')}
                  </StatusMessage>
                ) : null}
              </article>
            ))}
          </section>
        ) : null}

        {applyResults ? (
          <StatusMessage
            tone={applyResults.some((r) => r.status !== 'applied') ? 'warn' : 'success'}
            data-testid="cross-agent-apply-result"
          >
            <ul className={styles.userResultList}>
              {applyResults.map((row) => (
                <li key={`${row.destination}-${row.path}`}>
                  {t(`agentHub:targets.${row.destination}`)} · {row.status} · {row.path}
                  {row.errorCode ? ` (${row.errorCode})` : ''}
                </li>
              ))}
            </ul>
          </StatusMessage>
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
            variant="secondary"
            size="sm"
            loading={busy}
            disabled={!canPreview || busy}
            onClick={() => void runPreview()}
            data-testid="cross-agent-preview"
          >
            {t('agentHub:crossAgent.preview')}
          </Button>
          <Button
            variant="primary"
            size="sm"
            loading={busy}
            disabled={!preview || applicableCount === 0 || busy}
            onClick={() => void runApply()}
            data-testid="cross-agent-apply"
          >
            {t('agentHub:crossAgent.apply', { count: applicableCount })}
          </Button>
        </footer>
      </div>
    </Dialog>
  );
}
