/**
 * AgentLedgerDrawer — 本机 Agent 使用统计二级抽屉
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要查看自动汇总的 Agent 使用统计（次数/耗时/token），unknown 显示「未提供」而非 0；
 *   并需能发现「清除」入口（设置 → 常规），避免只在设置页看到清除、不知数据在哪看。
 *
 * Code Logic（这个组件做什么）:
 *   纯视图：接收 page/summary/loading 与回调；复用 Drawer/Button/Pill；
 *   底部提供跳转 `/settings?tab=general` 的清除入口；不 import @/api。
 */

import { useCallback, type ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { Button, Drawer, Pill } from '@/components/primitives';
import { formatTokenCount } from '@/lib/tokenFormat';
import { formatLocalDateTimeSeconds } from '@/lib/localDateTime';
import { splitDurationParts } from '@/lib/durationFormat';
import type {
  AgentLedgerEntry,
  AgentLedgerPage,
  AgentLedgerSummary,
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
 *   null token 必须显示「未提供」，禁止 0 tokens；大数需按 k/M 缩写提升可读性。
 *
 * Code Logic（这个函数做什么）:
 *   有限数字 → formatTokenCount（>5k 以 k、>=1M 以 M，3 位小数）；否则 i18n 未提供。
 */
function tokenLabel(
  value: number | null | undefined,
  unavailable: string,
): string {
  return formatTokenCount(value) ?? unavailable;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   时长原始值为毫秒,需按天/时/分/秒自动划分、精确到秒展示;
 *   各分量单位文案随 locale 切换,全零显示 0 秒。
 *
 * Code Logic（这个函数做什么）:
 *   splitDurationParts 拆分后跳过零分量(至少保留秒)用 i18n 单位拼接;
 *   非法时长返回「未提供」。
 */
function durationLabel(
  ms: number,
  unavailable: string,
  unitDay: string,
  unitHour: string,
  unitMinute: string,
  unitSecond: string,
): string {
  const parts = splitDurationParts(ms);
  if (!parts) return unavailable;
  const segments: string[] = [];
  if (parts.days > 0) segments.push(`${parts.days}${unitDay}`);
  if (parts.hours > 0) segments.push(`${parts.hours}${unitHour}`);
  if (parts.minutes > 0) segments.push(`${parts.minutes}${unitMinute}`);
  if (parts.seconds > 0 || segments.length === 0) {
    segments.push(`${parts.seconds}${unitSecond}`);
  }
  return segments.join(' ');
}

/**
 * Business Logic（为什么需要这个组件）:
 *   二级 drawer 呈现本机 Agent 使用统计，并互链设置清除入口。
 *
 * Code Logic（这个组件做什么）:
 *   Drawer + summary 块 + entry 列表 + load more + 底部跳转设置。
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
  const navigate = useNavigate();
  const titleId = 'agent-ledger-drawer-title';
  const unavailable = t('workbench:agentLedger.unavailable');

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户从查看页发现清除能力时，应一键落到设置常规页，而不是自己找。
   *
   * Code Logic（这个函数做什么）:
   *   关闭 drawer 后 navigate `/settings?tab=general`。
   */
  const openClearInSettings = useCallback(() => {
    onClose();
    navigate('/settings?tab=general');
  }, [navigate, onClose]);

  return (
    <Drawer
      open={open}
      onClose={onClose}
      titleId={titleId}
      side="right"
      className={styles.drawer}
    >
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
                <dd data-testid="ledger-duration">
                  {durationLabel(
                    summary.durationMs,
                    unavailable,
                    t('workbench:agentLedger.durationUnitDay'),
                    t('workbench:agentLedger.durationUnitHour'),
                    t('workbench:agentLedger.durationUnitMinute'),
                    t('workbench:agentLedger.durationUnitSecond'),
                  )}
                </dd>
              </div>
              <div>
                <dt>{t('workbench:agentLedger.cacheReadTokens')}</dt>
                <dd data-testid="ledger-cache-read-tokens">
                  {tokenLabel(summary.cacheReadTokens, unavailable)}
                </dd>
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

        <footer className={styles.footer} data-testid="agent-usage-stats-settings-link">
          <p className={styles.muted}>{t('workbench:agentLedger.clearInSettingsHint')}</p>
          <Button
            variant="secondary"
            size="sm"
            onClick={openClearInSettings}
            data-testid="agent-usage-stats-open-settings"
          >
            {t('workbench:agentLedger.openClearInSettings')}
          </Button>
        </footer>
      </div>
    </Drawer>
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   单行只展示 metadata，无 prompt/path；主标题为工作台终端窗口标题，
 *   无标题时回退 providerId，providerId 退为次要信息保留。
 *   outcome Pill 仅展示异常结果（failed/cancelled）——disconnected 是普通
 *   Workbench 终端会话的常见自然结束（用户关掉终端），逐行显示是噪音。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 标题(terminalTitle 回退 providerId)/providerId(有标题时)/异常 outcome Pill/tokens。
 */
function EntryRow({
  entry,
  unavailable,
}: {
  entry: AgentLedgerEntry;
  unavailable: string;
}): ReactElement {
  const { t } = useTranslation(['workbench']);
  const title = entry.terminalTitle?.trim() || null;
  const outcomePill =
    entry.outcome === 'failed' || entry.outcome === 'cancelled' ? (
      <Pill tone="neutral">{entry.outcome}</Pill>
    ) : null;
  return (
    <li className={styles.entryRow}>
      <div className={styles.entryMain}>
        <span className={styles.entryProvider}>{title ?? entry.providerId}</span>
        {title ? <span className={styles.entryProviderFallback}>{entry.providerId}</span> : null}
        {outcomePill}
      </div>
      <div className={styles.entryMeta}>
        <time dateTime={entry.endedAt}>{formatLocalDateTimeSeconds(entry.endedAt)}</time>
        <span>
          {t('workbench:agentLedger.cacheReadTokens')}:{' '}
          {tokenLabel(entry.cacheReadTokens, unavailable)}
        </span>
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
