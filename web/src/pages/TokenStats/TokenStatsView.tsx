/**
 * TokenStatsView - Token 统计页 pure view
 *
 * Business Logic（为什么需要这个组件）:
 *   把 Token 统计页所有展示逻辑（筛选 / KPI / 趋势图 / 三维分组 / 明细表 / 导出）
 *   集中在一个 pure view，禁止 import `@/api/*` 或 transport，与 controller 完全解耦。
 *
 * Code Logic（这个组件做什么）:
 *   - 顶部时间窗 / outcome / provider·model·project chips / 刷新 / 导出。
 *   - 8 个 KPI tile；null token 显示「—」并在 hint 标未提供。
 *   - recharts ComposedChart：input/output Bar + cacheRead/cost Line。
 *   - 三维分组 tab + cursor 分页明细。
 */

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Bar,
  CartesianGrid,
  ComposedChart,
  Line,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import { Button, Card, Pill, StatusMessage } from '@/components/primitives';
import { TokenIcon } from '@/lib/icons';
import { pickPrimaryCurrencyCost } from '@/lib/schemas/tokenStats';
import { formatTokenCount } from '@/lib/tokenFormat';
import type {
  AgentLedgerFilters,
  AgentLedgerGroupRow,
  AgentLedgerOutcome,
  AgentLedgerSessionEntry,
  AgentLedgerSummary,
  AgentLedgerTrendPoint,
  CurrencyAmount,
} from '@/lib/types/tokenStats';
import styles from './TokenStats.module.css';

/** view 使用的 controller 输出（避免 view 直接依赖 hook 文件）。 */
export interface TokenStatsViewProps {
  filter: AgentLedgerFilters;
  summary: AgentLedgerSummary | null;
  entries: AgentLedgerSessionEntry[];
  hasMore: boolean;
  loading: boolean;
  refreshError: 'idle' | 'loading' | 'stale' | 'error';
  exporting: boolean;
  exportError: string | null;
  lastExportPath: string | null;
  onChangeFilter(patch: Partial<AgentLedgerFilters>): void;
  onLoadMore(): void;
  onRefresh(): void;
  onExport(format: 'csv' | 'json'): void;
  onDismissExport(): void;
}

/** 趋势图派生行。 */
interface TrendChartRow {
  bucketStart: string;
  input: number;
  output: number;
  cacheRead: number;
  costMinor: number;
  currency: string;
}

/** recharts Tooltip 只读必要字段。 */
interface TrendTooltipProps {
  active?: boolean;
  payload?: Array<{ payload?: TrendChartRow }>;
  label?: string | number;
}

const OUTCOMES: AgentLedgerOutcome[] = ['completed', 'failed', 'cancelled', 'disconnected'];

/**
 * Business Logic（为什么需要）:
 *   表格与 KPI 要把 null 与 0 分开，避免把缺失用量显示成零消耗。
 *
 * Code Logic（做什么）:
 *   null/undefined → 占位符；其余走 formatTokenCount。
 */
function formatNumber(value: number | null | undefined): string {
  if (value == null) return '—';
  return formatTokenCount(value) ?? '0';
}

/**
 * Business Logic（为什么需要）:
 *   命中率是 0–1 小数，用户需要百分比。
 *
 * Code Logic（做什么）:
 *   null → 占位；否则保留 1 位小数。
 */
function formatPercent(value: number | null | undefined): string {
  if (value == null) return '—';
  return `${(value * 100).toFixed(1)}%`;
}

/**
 * Business Logic（为什么需要）:
 *   轴标签用短日期/小时，避免 RFC3339 挤爆横轴。
 *
 * Code Logic（做什么）:
 *   hour 取 HH:MM，day 取 MM-DD；均按 UTC 子串，不转本地时区。
 */
function formatBucketLabel(iso: string, bucket: 'hour' | 'day'): string {
  const hh = iso.length >= 13 ? iso.slice(11, 16) : '';
  const md = iso.length >= 10 ? iso.slice(5, 10) : iso;
  return bucket === 'hour' ? hh || md : md;
}

/**
 * Business Logic（为什么需要）:
 *   分组表成本列只展示主货币，避免多币种折算。
 *
 * Code Logic（做什么）:
 *   pickPrimaryCurrencyCost + minorUnits/100。
 */
function bucketToMinorLabel(rows: CurrencyAmount[], locale: string): string {
  if (rows.length === 0) return '—';
  const primary = pickPrimaryCurrencyCost(rows, locale);
  if (primary.primary == null) return '—';
  return `${(primary.primary.minorUnits / 100).toFixed(2)} ${primary.primary.currency}`;
}

/**
 * Business Logic（为什么需要）:
 *   多选 chips 需要在当前选中值与 summary 分组键之间取并集，筛选后选项不会丢。
 *
 * Code Logic（做什么）:
 *   selected ∪ row.key，保持插入顺序。
 */
function chipOptions(
  rows: AgentLedgerGroupRow[] | undefined,
  selected: string[] | null | undefined,
): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const id of selected ?? []) {
    if (!seen.has(id)) {
      seen.add(id);
      out.push(id);
    }
  }
  for (const row of rows ?? []) {
    if (!seen.has(row.key)) {
      seen.add(row.key);
      out.push(row.key);
    }
  }
  return out;
}

/**
 * Business Logic（为什么需要）:
 *   provider/model/project 多选是加减集合，空集应回到「不筛选」。
 *
 * Code Logic（做什么）:
 *   已存在则删除，否则追加；空数组归一成 null。
 */
function toggleId(current: string[] | null | undefined, id: string): string[] | null {
  const cur = current ?? [];
  const next = cur.includes(id) ? cur.filter((item) => item !== id) : [...cur, id];
  return next.length === 0 ? null : next;
}

/**
 * Business Logic（为什么需要）:
 *   KPI 需要独立视觉块，避免 8 个指标挤在一段文字里。
 *
 * Code Logic（做什么）:
 *   渲染 label/value/hint；可选 accent 与 testid。
 */
function KpiTile({
  label,
  value,
  hint,
  tone = 'default',
  testId,
}: {
  label: string;
  value: string;
  hint?: string;
  tone?: 'default' | 'accent';
  testId?: string;
}) {
  return (
    <article className={styles.kpiTile} data-testid={testId}>
      <span className={styles.kpiLabel}>{label}</span>
      <strong className={tone === 'accent' ? styles.kpiValueAccent : styles.kpiValue}>{value}</strong>
      {hint ? <span className={styles.kpiHint}>{hint}</span> : null}
    </article>
  );
}

/**
 * Business Logic（为什么需要）:
 *   Token 统计页的主展示面：筛选后立刻能看到 KPI、趋势、拆分和明细。
 *
 * Code Logic（做什么）:
 *   消费 controller props 渲染；group tab / export menu 为本地 UI 状态。
 */
export function TokenStatsView({
  filter,
  summary,
  entries,
  hasMore,
  loading,
  refreshError,
  exporting,
  exportError,
  lastExportPath,
  onChangeFilter,
  onLoadMore,
  onRefresh,
  onExport,
  onDismissExport,
}: TokenStatsViewProps) {
  const { t, i18n } = useTranslation(['tokenStats', 'common']);
  const [groupTab, setGroupTab] = useState<'model' | 'provider' | 'project'>('model');
  const [exportMenuOpen, setExportMenuOpen] = useState(false);

  const currentByTab: AgentLedgerGroupRow[] = useMemo(() => {
    if (!summary) return [];
    if (groupTab === 'model') return summary.byModel;
    if (groupTab === 'provider') return summary.byProvider;
    return summary.byProject;
  }, [summary, groupTab]);

  const locale = i18n.language || 'en';
  const totalCost = useMemo(() => {
    if (!summary) return { label: '—', others: [] as CurrencyAmount[] };
    const picked = pickPrimaryCurrencyCost(summary.totalCostByCurrency, locale);
    if (picked.primary == null) return { label: '—', others: picked.others };
    return {
      label: `${(picked.primary.minorUnits / 100).toFixed(2)} ${picked.primary.currency}`,
      others: picked.others,
    };
  }, [summary, locale]);

  const trendData: TrendChartRow[] = useMemo(() => {
    if (!summary) return [];
    return summary.trend.map((point: AgentLedgerTrendPoint) => {
      const picked = pickPrimaryCurrencyCost(point.costByCurrency, locale);
      const minor = picked.primary ?? point.costByCurrency[0] ?? null;
      return {
        bucketStart: point.bucketStart,
        input: point.inputTokens ?? 0,
        output: point.outputTokens ?? 0,
        cacheRead: point.cacheReadTokens ?? 0,
        costMinor: minor ? minor.minorUnits : 0,
        currency: minor ? minor.currency : 'USD',
      };
    });
  }, [summary, locale]);

  const bucket = summary?.bucket ?? 'day';

  return (
    <div className={styles.page} data-testid="token-stats-page">
      <div className={styles.container}>
        <header className={styles.header}>
          <div className={styles.headerText}>
            <span className={styles.eyebrow}>{t('tokenStats:eyebrow')}</span>
            <h1 className={styles.title}>{t('tokenStats:title')}</h1>
            <p className={styles.lead}>{t('tokenStats:lead')}</p>
          </div>
          <div className={styles.headerActions}>
            <Button variant="secondary" size="sm" onClick={onRefresh} data-testid="token-stats-refresh">
              {t('tokenStats:filters.refresh')}
            </Button>
            <ExportMenu
              open={exportMenuOpen}
              onOpenChange={setExportMenuOpen}
              exporting={exporting}
              onExport={(fmt) => {
                onExport(fmt);
                setExportMenuOpen(false);
              }}
            />
          </div>
        </header>

        <FilterBar filter={filter} summary={summary} onChange={onChangeFilter} />

        {refreshError === 'stale' ? (
          <div className={styles.staleBanner} role="status" data-testid="token-stats-stale-banner">
            <p className={styles.staleText}>{t('tokenStats:session.staleBanner')}</p>
            <Button variant="secondary" size="sm" onClick={onRefresh}>
              {t('common:action.retry')}
            </Button>
          </div>
        ) : null}

        <Card variant="outlined" padding="md">
          <Card.Body>
            {loading && summary == null ? (
              <div className={styles.chartEmpty}>{t('tokenStats:kpi.loading')}</div>
            ) : (
              <section className={styles.kpiGrid} aria-label={t('tokenStats:title')}>
                <KpiTile
                  testId="token-stats-kpi-input"
                  label={t('tokenStats:kpi.inputTokens')}
                  value={formatNumber(summary?.inputTokens)}
                  hint={summary?.inputTokens == null ? t('tokenStats:kpi.notProvided') : undefined}
                />
                <KpiTile
                  testId="token-stats-kpi-cache"
                  label={t('tokenStats:kpi.cacheTokens')}
                  value={formatNumber(summary?.cacheReadTokens)}
                  hint={summary?.cacheReadTokens == null ? t('tokenStats:kpi.notProvided') : undefined}
                />
                <KpiTile
                  testId="token-stats-kpi-output"
                  label={t('tokenStats:kpi.outputTokens')}
                  value={formatNumber(summary?.outputTokens)}
                  hint={summary?.outputTokens == null ? t('tokenStats:kpi.notProvided') : undefined}
                />
                <KpiTile
                  testId="token-stats-kpi-hit-rate"
                  label={t('tokenStats:kpi.cacheHitRate')}
                  value={formatPercent(summary?.cacheHitRate)}
                  tone="accent"
                  hint={summary?.cacheHitRate == null ? t('tokenStats:kpi.notProvided') : undefined}
                />
                <KpiTile
                  testId="token-stats-kpi-real"
                  label={t('tokenStats:kpi.realConsumed')}
                  value={formatNumber(summary?.realConsumedTokens)}
                  hint={summary?.realConsumedTokens == null ? t('tokenStats:kpi.notProvided') : undefined}
                />
                <KpiTile
                  testId="token-stats-kpi-requests"
                  label={t('tokenStats:kpi.requests')}
                  value={summary?.requestsCount != null ? String(summary.requestsCount) : '—'}
                />
                <KpiTile
                  testId="token-stats-kpi-cost"
                  label={t('tokenStats:kpi.totalCost')}
                  value={totalCost.label}
                  hint={
                    totalCost.others.length > 0
                      ? totalCost.others
                          .map((row) => `${(row.minorUnits / 100).toFixed(2)} ${row.currency}`)
                          .join(' · ')
                      : undefined
                  }
                />
                <KpiTile
                  testId="token-stats-kpi-coverage"
                  label={t('tokenStats:kpi.coverage')}
                  value={
                    summary
                      ? summary.usageCoverage === 'complete'
                        ? t('tokenStats:kpi.coverageComplete')
                        : summary.usageCoverage === 'partial'
                          ? t('tokenStats:kpi.coveragePartial')
                          : t('tokenStats:kpi.coverageUnavailable')
                      : '—'
                  }
                />
              </section>
            )}
          </Card.Body>
        </Card>

        <Card variant="outlined" padding="md" className={styles.chartCard}>
          <Card.Body className={styles.chartBody}>
            <h2 className={styles.groupTitle}>{t('tokenStats:trend.title')}</h2>
            {trendData.length === 0 ? (
              <div className={styles.chartEmpty} data-testid="token-stats-trend-empty">
                {t('tokenStats:trend.empty')}
              </div>
            ) : (
              <div data-testid="token-stats-trend">
                <ResponsiveContainer width="100%" height={280}>
                  <ComposedChart data={trendData} margin={{ top: 12, right: 12, bottom: 8, left: 0 }}>
                    <CartesianGrid stroke="var(--border-soft)" strokeDasharray="2 4" />
                    <XAxis
                      dataKey="bucketStart"
                      tickFormatter={(value: string) => formatBucketLabel(value, bucket)}
                      stroke="var(--fg-muted-readable)"
                      tickLine={false}
                    />
                    <YAxis
                      yAxisId="tokens"
                      stroke="var(--fg-muted-readable)"
                      tickLine={false}
                      tickFormatter={(value: number) => formatTokenCount(value) ?? '0'}
                    />
                    <YAxis
                      yAxisId="cost"
                      orientation="right"
                      stroke="var(--fg-muted-readable)"
                      tickLine={false}
                      tickFormatter={(value: number) => (value / 100).toFixed(2)}
                    />
                    <Tooltip content={<TrendTooltip />} />
                    <Bar
                      yAxisId="tokens"
                      dataKey="input"
                      name={t('tokenStats:trend.legend.input')}
                      fill="var(--accent)"
                      radius={[3, 3, 0, 0]}
                    />
                    <Bar
                      yAxisId="tokens"
                      dataKey="output"
                      name={t('tokenStats:trend.legend.output')}
                      fill="var(--success)"
                      radius={[3, 3, 0, 0]}
                    />
                    <Line
                      yAxisId="tokens"
                      type="monotone"
                      dataKey="cacheRead"
                      name={t('tokenStats:trend.legend.cacheRead')}
                      stroke="var(--warn)"
                      strokeWidth={1.6}
                      dot={false}
                    />
                    <Line
                      yAxisId="cost"
                      type="monotone"
                      dataKey="costMinor"
                      name={t('tokenStats:trend.legend.cost')}
                      stroke="var(--fg-2)"
                      strokeWidth={1.4}
                      dot={false}
                    />
                  </ComposedChart>
                </ResponsiveContainer>
              </div>
            )}
          </Card.Body>
        </Card>

        <Card variant="outlined" padding="md">
          <Card.Body>
            <div className={styles.tabsBar} role="tablist" aria-label={t('tokenStats:title')}>
              <button
                type="button"
                role="tab"
                aria-selected={groupTab === 'model'}
                className={groupTab === 'model' ? `${styles.tabButton} ${styles.tabButtonActive}` : styles.tabButton}
                onClick={() => setGroupTab('model')}
              >
                {t('tokenStats:groups.byModel')}
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={groupTab === 'provider'}
                className={
                  groupTab === 'provider' ? `${styles.tabButton} ${styles.tabButtonActive}` : styles.tabButton
                }
                onClick={() => setGroupTab('provider')}
              >
                {t('tokenStats:groups.byProvider')}
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={groupTab === 'project'}
                className={groupTab === 'project' ? `${styles.tabButton} ${styles.tabButtonActive}` : styles.tabButton}
                onClick={() => setGroupTab('project')}
              >
                {t('tokenStats:groups.byProject')}
              </button>
            </div>
            {currentByTab.length === 0 ? (
              <p className={styles.groupEmpty}>{t('tokenStats:groups.noRows')}</p>
            ) : (
              <GroupTable rows={currentByTab} locale={locale} />
            )}
          </Card.Body>
        </Card>

        <Card variant="outlined" padding="md">
          <Card.Body>
            <h2 className={styles.groupTitle}>{t('tokenStats:session.tableTitle')}</h2>
            {entries.length === 0 ? (
              <p className={styles.groupEmpty}>{t('tokenStats:session.empty')}</p>
            ) : (
              <SessionTable rows={entries} />
            )}
            {hasMore ? (
              <div className={styles.sessionActions}>
                <Button variant="secondary" size="sm" onClick={onLoadMore} data-testid="token-stats-load-more">
                  {t('tokenStats:session.loadMore')}
                </Button>
              </div>
            ) : null}
          </Card.Body>
        </Card>
      </div>

      {exportError || lastExportPath ? (
        <div className={styles.toast} aria-live="polite" data-testid="token-stats-export-toast">
          {exportError ? (
            <StatusMessage
              tone="danger"
              action={
                <Button variant="secondary" size="sm" onClick={onDismissExport}>
                  {t('tokenStats:export.close')}
                </Button>
              }
            >
              {t('tokenStats:export.failure', { message: exportError })}
            </StatusMessage>
          ) : lastExportPath ? (
            <StatusMessage
              tone="success"
              action={
                <Button variant="secondary" size="sm" onClick={onDismissExport}>
                  {t('tokenStats:export.close')}
                </Button>
              }
            >
              {t('tokenStats:export.success', { path: lastExportPath })}
            </StatusMessage>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

function FilterBar({
  filter,
  summary,
  onChange,
}: {
  filter: AgentLedgerFilters;
  summary: AgentLedgerSummary | null;
  onChange: (patch: Partial<AgentLedgerFilters>) => void;
}) {
  const { t } = useTranslation(['tokenStats']);
  const windows: Array<{ key: '24h' | '7d' | '30d'; label: string }> = [
    { key: '24h', label: t('tokenStats:filters.window.24h') },
    { key: '7d', label: t('tokenStats:filters.window.7d') },
    { key: '30d', label: t('tokenStats:filters.window.30d') },
  ];
  const providers = chipOptions(summary?.byProvider, filter.providerIds);
  const models = chipOptions(summary?.byModel, filter.modelIds);
  const projects = chipOptions(summary?.byProject, filter.projectIds);

  return (
    <div className={styles.filterBar} data-testid="token-stats-filters">
      <div className={styles.filterGroup} role="group" aria-label={t('tokenStats:filters.windowLabel')}>
        {windows.map((item) => (
          <Button
            key={item.key}
            variant={filter.window === item.key ? 'primary' : 'secondary'}
            size="sm"
            data-testid={`token-stats-window-${item.key}`}
            onClick={() => onChange({ window: item.key })}
          >
            {item.label}
          </Button>
        ))}
      </div>
      <label className={styles.filterLabel}>
        <span>{t('tokenStats:filters.outcomeLabel')}</span>
        <select
          className={styles.filterSelect}
          data-testid="token-stats-filter-outcome"
          aria-label={t('tokenStats:filters.outcomeLabel')}
          value={filter.outcome ?? ''}
          onChange={(event) => {
            const next = event.target.value;
            onChange({ outcome: next === '' ? null : (next as AgentLedgerOutcome) });
          }}
        >
          <option value="">{t('tokenStats:filters.outcome.all')}</option>
          {OUTCOMES.map((outcome) => (
            <option key={outcome} value={outcome}>
              {t(`tokenStats:filters.outcome.${outcome}`)}
            </option>
          ))}
        </select>
      </label>
      {providers.length > 0 ? (
        <ChipFilter
          label={t('tokenStats:filters.providerLabel')}
          options={providers}
          selected={filter.providerIds}
          onToggle={(id) => onChange({ providerIds: toggleId(filter.providerIds, id) })}
        />
      ) : null}
      {models.length > 0 ? (
        <ChipFilter
          label={t('tokenStats:filters.modelLabel')}
          options={models}
          selected={filter.modelIds}
          onToggle={(id) => onChange({ modelIds: toggleId(filter.modelIds, id) })}
        />
      ) : null}
      {projects.length > 0 ? (
        <ChipFilter
          label={t('tokenStats:filters.projectLabel')}
          options={projects}
          selected={filter.projectIds}
          onToggle={(id) => onChange({ projectIds: toggleId(filter.projectIds, id) })}
        />
      ) : null}
      <Button
        variant="ghost"
        size="sm"
        data-testid="token-stats-filter-reset"
        onClick={() =>
          onChange({
            window: '7d',
            providerIds: null,
            modelIds: null,
            projectIds: null,
            outcome: null,
            startedAfter: null,
            startedBefore: null,
          })
        }
      >
        {t('tokenStats:filters.reset')}
      </Button>
    </div>
  );
}

function ChipFilter({
  label,
  options,
  selected,
  onToggle,
}: {
  label: string;
  options: string[];
  selected: string[] | null | undefined;
  onToggle: (id: string) => void;
}) {
  const active = selected ?? [];
  return (
    <div className={styles.filterGroup} role="group" aria-label={label}>
      <span className={styles.chipLabel}>{label}</span>
      {options.map((id) => (
        <Button
          key={id}
          variant={active.includes(id) ? 'primary' : 'secondary'}
          size="sm"
          onClick={() => onToggle(id)}
        >
          {id}
        </Button>
      ))}
    </div>
  );
}

function ExportMenu({
  open,
  onOpenChange,
  exporting,
  onExport,
}: {
  open: boolean;
  onOpenChange: (next: boolean) => void;
  exporting: boolean;
  onExport: (fmt: 'csv' | 'json') => void;
}) {
  const { t } = useTranslation(['tokenStats']);
  return (
    <div className={styles.exportMenuRoot}>
      <Button
        variant="secondary"
        size="sm"
        icon={<TokenIcon />}
        onClick={() => onOpenChange(!open)}
        disabled={exporting}
        aria-haspopup="menu"
        aria-expanded={open}
        data-testid="token-stats-export-menu"
      >
        {t('tokenStats:filters.exportMenu')}
      </Button>
      {open ? (
        <div className={styles.exportPopover} role="menu">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => onExport('csv')}
            disabled={exporting}
            data-testid="token-stats-export-csv"
          >
            {t('tokenStats:filters.exportCsv')}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => onExport('json')}
            disabled={exporting}
            data-testid="token-stats-export-json"
          >
            {t('tokenStats:filters.exportJson')}
          </Button>
        </div>
      ) : null}
    </div>
  );
}

function GroupTable({ rows, locale }: { rows: AgentLedgerGroupRow[]; locale: string }) {
  const { t } = useTranslation(['tokenStats']);
  return (
    <table className={styles.table} data-testid="token-stats-group-table">
      <thead>
        <tr>
          <th>{t('tokenStats:groups.columns.key')}</th>
          <th>{t('tokenStats:groups.columns.sessions')}</th>
          <th>{t('tokenStats:groups.columns.input')}</th>
          <th>{t('tokenStats:groups.columns.cacheRead')}</th>
          <th>{t('tokenStats:groups.columns.cacheWrite')}</th>
          <th>{t('tokenStats:groups.columns.output')}</th>
          <th>{t('tokenStats:groups.columns.cost')}</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((row) => (
          <tr key={row.key}>
            <td>{row.label ?? row.key}</td>
            <td>
              {row.sessions}
              {row.failed > 0 ? (
                <>
                  {' '}
                  <Pill tone="danger">{row.failed}</Pill>
                </>
              ) : null}
            </td>
            <td>{formatNumber(row.inputTokens)}</td>
            <td>{formatNumber(row.cacheReadTokens)}</td>
            <td>{formatNumber(row.cacheWriteTokens)}</td>
            <td>{formatNumber(row.outputTokens)}</td>
            <td>{bucketToMinorLabel(row.costByCurrency, locale)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function SessionTable({ rows }: { rows: AgentLedgerSessionEntry[] }) {
  const { t, i18n } = useTranslation(['tokenStats']);
  const fmt = new Intl.DateTimeFormat(i18n.language || 'en', {
    dateStyle: 'short',
    timeStyle: 'short',
  });
  return (
    <table className={styles.table} data-testid="token-stats-session-table">
      <thead>
        <tr>
          <th>{t('tokenStats:session.columns.startedAt')}</th>
          <th>{t('tokenStats:session.columns.agent')}</th>
          <th>{t('tokenStats:session.columns.model')}</th>
          <th>{t('tokenStats:session.columns.project')}</th>
          <th>{t('tokenStats:session.columns.outcome')}</th>
          <th>{t('tokenStats:session.columns.input')}</th>
          <th>{t('tokenStats:session.columns.cacheRead')}</th>
          <th>{t('tokenStats:session.columns.output')}</th>
          <th>{t('tokenStats:session.columns.cost')}</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((row) => (
          <tr key={row.id}>
            <td>{fmt.format(new Date(row.startedAt))}</td>
            <td>{row.providerId}</td>
            <td>{row.modelId ?? '—'}</td>
            <td>{row.projectId}</td>
            <td>{t(`tokenStats:filters.outcome.${row.outcome}`)}</td>
            <td>{formatNumber(row.inputTokens)}</td>
            <td>{formatNumber(row.cacheReadTokens)}</td>
            <td>{formatNumber(row.outputTokens)}</td>
            <td>
              {row.costMinorUnits != null && row.costCurrency
                ? `${(row.costMinorUnits / 100).toFixed(2)} ${row.costCurrency}`
                : '—'}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function TrendTooltip({ active, payload, label }: TrendTooltipProps) {
  const { t } = useTranslation(['tokenStats']);
  if (!active || !payload?.length) return null;
  const point = payload[0]?.payload;
  return (
    <div className={styles.tooltip}>
      <div className={styles.tooltipLabel}>{label}</div>
      <div className={styles.tooltipRow}>
        <span>{t('tokenStats:trend.legend.input')}</span>
        <strong>{formatTokenCount(point?.input ?? 0)}</strong>
      </div>
      <div className={styles.tooltipRow}>
        <span>{t('tokenStats:trend.legend.output')}</span>
        <strong>{formatTokenCount(point?.output ?? 0)}</strong>
      </div>
      <div className={styles.tooltipRow}>
        <span>{t('tokenStats:trend.legend.cacheRead')}</span>
        <strong>{formatTokenCount(point?.cacheRead ?? 0)}</strong>
      </div>
      <div className={styles.tooltipRow}>
        <span>{t('tokenStats:trend.legend.cost')}</span>
        <strong>{point ? `${((point.costMinor ?? 0) / 100).toFixed(2)} ${point.currency ?? ''}` : '—'}</strong>
      </div>
    </div>
  );
}
