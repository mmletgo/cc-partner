/**
 * AgentLedgerDrawer — 本机 Agent metadata 历史二级抽屉
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要查看自动产生的 metadata-only 历史与时间窗用量，unknown 显示「未提供」而非 0。
 *
 * Code Logic（这个组件做什么）:
 *   纯视图：接收 page/summary/loading 与回调；复用 Drawer/Button/Pill；不 import @/api。
 */

import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Drawer, Pill } from '@/components/primitives';
import type {
  AgentLedgerEntry,
  AgentLedgerPage,
  AgentLedgerSummary,
  LedgerUsageCoverage,
} from '@/lib/types/agentLedger';
import styles from './AgentLedgerDrawer.module.css';

export interface AgentLedgerDrawerProps {
  open: boolean;
  onClose: () => void;
  /** 本机 local 项目才可看明细；remote 时显示不可用提示 */
  localOnlyAvailable: boolean;
  page: AgentLedgerPage | null;
  summary: AgentLedgerSummary | null;
  loading?: boolean;
  error?: string | null;
  loadingMore?: boolean;
  onLoadMore?: () => void;
  onRefresh?: () => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   null token 必须显示「未提供」，禁止 0 tokens。
 *
 * Code Logic（这个函数做什么）:
 *   有限数字 → 文本；否则 i18n 未提供。
 */
function tokenLabel(
  value: number | null | undefined,
  unavailable: string,
): string {
  if (value == null || !Number.isFinite(value)) return unavailable;
  return String(value);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   coverage 需要可读标签。
 *
 * Code Logic（这个函数做什么）:
 *   映射 coverage → i18n key。
 */
function coverageTone(coverage: LedgerUsageCoverage): 'success' | 'warn' | 'neutral' {
  if (coverage === 'complete') return 'success';
  if (coverage === 'partial') return 'warn';
  return 'neutral';
}

/**
 * Business Logic（为什么需要这个组件）:
 *   二级 drawer 呈现本机 Agent metadata 历史。
 *
 * Code Logic（这个组件做什么）:
 *   Drawer + summary 块 + entry 列表 + load more。
 */
export function AgentLedgerDrawer({
  open,
  onClose,
  localOnlyAvailable,
  page,
  summary,
  loading = false,
  error = null,
  loadingMore = false,
  onLoadMore,
  onRefresh,
}: AgentLedgerDrawerProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const titleId = 'agent-ledger-drawer-title';
  const unavailable = t('workbench:agentLedger.unavailable');

  return (
    <Drawer open={open} onClose={onClose} titleId={titleId} side="right">
      <div className={styles.root}>
        <header className={styles.header}>
          <h2 id={titleId} className={styles.title}>
            {t('workbench:agentLedger.title')}
          </h2>
          <div className={styles.headerActions}>
            {onRefresh ? (
              <Button variant="ghost" size="sm" onClick={onRefresh} disabled={loading}>
                {t('workbench:refresh')}
              </Button>
            ) : null}
            <Button variant="ghost" size="sm" onClick={onClose}>
              {t('workbench:agentLedger.close')}
            </Button>
          </div>
        </header>

        <p className={styles.subtitle}>{t('workbench:agentLedger.subtitle')}</p>

        {!localOnlyAvailable ? (
          <p className={styles.state} role="status">
            {t('workbench:agentLedger.localOnly')}
          </p>
        ) : null}

        {error ? (
          <p className={styles.error} role="alert">
            {error}
          </p>
        ) : null}

        {localOnlyAvailable && summary ? (
          <section className={styles.summary} aria-label={t('workbench:agentLedger.summaryTitle')}>
            <div className={styles.summaryHeader}>
              <h3 className={styles.sectionTitle}>
                {t('workbench:agentLedger.summaryTitle')} · {summary.window}
              </h3>
              <Pill tone={coverageTone(summary.usageCoverage)}>
                {t(`workbench:agentLedger.coverage.${summary.usageCoverage}`)}
              </Pill>
            </div>
            <dl className={styles.summaryGrid}>
              <div>
                <dt>{t('workbench:agentLedger.sessions')}</dt>
                <dd>{summary.sessions}</dd>
              </div>
              <div>
                <dt>{t('workbench:agentLedger.completed')}</dt>
                <dd>{summary.completed}</dd>
              </div>
              <div>
                <dt>{t('workbench:agentLedger.failed')}</dt>
                <dd>{summary.failed}</dd>
              </div>
              <div>
                <dt>{t('workbench:agentLedger.duration')}</dt>
                <dd>{summary.durationMs} ms</dd>
              </div>
              <div>
                <dt>{t('workbench:agentLedger.inputTokens')}</dt>
                <dd data-testid="ledger-input-tokens">
                  {tokenLabel(summary.inputTokens, unavailable)}
                </dd>
              </div>
              <div>
                <dt>{t('workbench:agentLedger.outputTokens')}</dt>
                <dd data-testid="ledger-output-tokens">
                  {tokenLabel(summary.outputTokens, unavailable)}
                </dd>
              </div>
            </dl>
            {summary.costByCurrency.length > 0 ? (
              <ul className={styles.costList}>
                {summary.costByCurrency.map((c) => (
                  <li key={c.currency}>
                    {c.currency}: {c.minorUnits}
                  </li>
                ))}
              </ul>
            ) : (
              <p className={styles.muted}>{t('workbench:agentLedger.costUnavailable')}</p>
            )}
          </section>
        ) : null}

        {localOnlyAvailable ? (
          <section className={styles.listSection} aria-label={t('workbench:agentLedger.listTitle')}>
            <h3 className={styles.sectionTitle}>{t('workbench:agentLedger.listTitle')}</h3>
            {loading && !page ? (
              <p className={styles.state}>{t('workbench:loading')}</p>
            ) : null}
            {page && page.items.length === 0 ? (
              <p className={styles.state}>{t('workbench:agentLedger.empty')}</p>
            ) : null}
            <ul className={styles.entryList}>
              {(page?.items ?? []).map((entry) => (
                <EntryRow key={entry.id} entry={entry} unavailable={unavailable} />
              ))}
            </ul>
            {page?.nextCursor && onLoadMore ? (
              <Button
                variant="secondary"
                size="sm"
                onClick={onLoadMore}
                loading={loadingMore}
                disabled={loadingMore}
              >
                {t('workbench:agentLedger.loadMore')}
              </Button>
            ) : null}
          </section>
        ) : null}
      </div>
    </Drawer>
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   单行只展示 metadata，无 prompt/path。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 provider/outcome/tokens。
 */
function EntryRow({
  entry,
  unavailable,
}: {
  entry: AgentLedgerEntry;
  unavailable: string;
}): ReactElement {
  const { t } = useTranslation(['workbench']);
  return (
    <li className={styles.entryRow}>
      <div className={styles.entryMain}>
        <span className={styles.entryProvider}>{entry.providerId}</span>
        <Pill tone="neutral">{entry.outcome}</Pill>
      </div>
      <div className={styles.entryMeta}>
        <time dateTime={entry.endedAt}>{entry.endedAt}</time>
        <span>
          {t('workbench:agentLedger.inputTokens')}:{' '}
          {tokenLabel(entry.inputTokens, unavailable)}
        </span>
        <span>
          {t('workbench:agentLedger.outputTokens')}:{' '}
          {tokenLabel(entry.outputTokens, unavailable)}
        </span>
        {entry.modelId ? <span>{entry.modelId}</span> : null}
      </div>
    </li>
  );
}
