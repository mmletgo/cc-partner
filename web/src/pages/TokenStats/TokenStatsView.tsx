/**
 * TokenStatsView - Token 统计页 pure view
 *
 * Business Logic（为什么需要这个组件）:
 *   把 Token 统计页所有展示逻辑（筛选 / KPI / 趋势图 / 明细表 / 导出）
 *   集中在一个 pure view，禁止 import `@/api/*` 或 transport，与 controller 完全解耦。
 *   按筛选条件直接显示汇总结果（KPI + Trend + Session），不再单独渲染 byModel /
 *   byProvider / byProject 三维分组表；Session 明细按 projectName 优先展示，缺失时
 *   回落到 projectId。
 *
 * Code Logic（这个组件做什么）:
 *   - 顶部时间窗 / provider·model·project 各占一行；后三类为多选 chips（空选 = 全部）。
 *   - 8 个 KPI tile；null token 显示「—」并在 hint 标未提供。
 *   - 用量趋势拆成 3 张图（新输入 / 缓存 / 输出），每张只保留 token 数纵轴。
 *   - Session 明细表：项目列显示 projectName ?? projectId；无 outcome 列。
 */

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  CartesianGrid,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import { Button, Card, StatusMessage } from '@/components/primitives';
import { TokenIcon } from '@/lib/icons';
import { formatLocalBucketLabel, formatLocalBucketTooltip } from '@/lib/localDateTime';
import { pickPrimaryCurrencyCost } from '@/lib/schemas/tokenStats';
import { formatTokenCount } from '@/lib/tokenFormat';
import type {
  AgentLedgerFilters,
  AgentLedgerGroupRow,
  AgentLedgerSessionEntry,
  AgentLedgerSummary,
  AgentLedgerTrendPoint,
  CurrencyAmount,
  TokenStatsFacetOptions,
} from '@/lib/types/tokenStats';
import styles from './TokenStats.module.css';
import { datetimeLocalFromRfc3339, rfc3339FromDatetimeLocal } from './tokenStatsTime';

/** view 使用的 controller 输出（避免 view 直接依赖 hook 文件）。 */
export interface TokenStatsViewProps {
  filter: AgentLedgerFilters;
  summary: AgentLedgerSummary | null;
  /** 时间窗内完整选项；缺省时回落到 summary 分组，避免测试样例必填。 */
  facetOptions?: TokenStatsFacetOptions | null;
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
  onRevealExport(): void;
  onDismissExport(): void;
}

/** 趋势图派生行。 */
interface TrendChartRow {
  bucketStart: string;
  input: number;
  output: number;
  cacheRead: number;
}

/** chip 展示项：id 用于筛选，label 用于按钮文案。 */
interface ChipOption {
  id: string;
  label: string;
}

/** 单张趋势图绑定的 token 字段。 */
type TrendMetricKey = 'input' | 'cacheRead' | 'output';

/** recharts Tooltip 只读必要字段。 */
interface TrendTooltipProps {
  active?: boolean;
  payload?: Array<{ payload?: TrendChartRow; value?: number }>;
  label?: string | number;
  bucket: 'hour' | 'day';
  metric: TrendMetricKey;
}

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
 *   多选 chips 需要在当前选中值与 summary 分组键之间取并集，筛选后选项不会丢。
 *
 * Code Logic（做什么）:
 *   selected ∪ row.key，保持插入顺序。
 */
function chipOptions(
  rows: AgentLedgerGroupRow[] | undefined,
  selected: string[] | null | undefined,
): ChipOption[] {
  const seen = new Set<string>();
  const out: ChipOption[] = [];
  for (const id of selected ?? []) {
    if (!seen.has(id)) {
      seen.add(id);
      out.push({ id, label: id });
    }
  }
  for (const row of rows ?? []) {
    if (!seen.has(row.key)) {
      seen.add(row.key);
      out.push({ id: row.key, label: row.label?.trim() || row.key });
    } else {
      const existing = out.find((item) => item.id === row.key);
      if (existing && row.label?.trim()) existing.label = row.label.trim();
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
 *   Token 统计页的主展示面：筛选后立刻能看到 KPI、趋势与 Session 明细。
 *
 * Code Logic（做什么）:
 *   消费 controller props 渲染；当前不再展示 byModel/byProvider/byProject 分组表，
 *   导出菜单为本地 UI 状态。
 */
export function TokenStatsView({
  filter,
  summary,
  facetOptions,
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
  onRevealExport,
  onDismissExport,
}: TokenStatsViewProps) {
  const { t, i18n } = useTranslation(['tokenStats', 'common']);
  const [exportMenuOpen, setExportMenuOpen] = useState(false);

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
    return summary.trend.map((point: AgentLedgerTrendPoint) => ({
      bucketStart: point.bucketStart,
      input: point.inputTokens ?? 0,
      output: point.outputTokens ?? 0,
      cacheRead: point.cacheReadTokens ?? 0,
    }));
  }, [summary]);

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

        <FilterBar
          filter={filter}
          summary={summary}
          facetOptions={facetOptions}
          onChange={onChangeFilter}
        />

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
              <div className={styles.trendStack} data-testid="token-stats-trend">
                <TrendMetricChart
                  testId="token-stats-trend-input"
                  title={t('tokenStats:trend.charts.input')}
                  data={trendData}
                  bucket={bucket}
                  metric="input"
                  stroke="var(--accent)"
                />
                <TrendMetricChart
                  testId="token-stats-trend-cache"
                  title={t('tokenStats:trend.charts.cache')}
                  data={trendData}
                  bucket={bucket}
                  metric="cacheRead"
                  stroke="var(--warn)"
                />
                <TrendMetricChart
                  testId="token-stats-trend-output"
                  title={t('tokenStats:trend.charts.output')}
                  data={trendData}
                  bucket={bucket}
                  metric="output"
                  stroke="var(--success)"
                />
              </div>
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
          {exportError && lastExportPath ? (
            <StatusMessage
              tone="danger"
              action={
                <>
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={onRevealExport}
                    data-testid="token-stats-export-reveal"
                  >
                    {t('tokenStats:export.reveal')}
                  </Button>
                  <Button variant="secondary" size="sm" onClick={onDismissExport}>
                    {t('tokenStats:export.close')}
                  </Button>
                </>
              }
            >
              {t('tokenStats:export.revealFailed', { message: exportError })}
            </StatusMessage>
          ) : exportError ? (
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
                <>
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={onRevealExport}
                    data-testid="token-stats-export-reveal"
                  >
                    {t('tokenStats:export.reveal')}
                  </Button>
                  <Button variant="secondary" size="sm" onClick={onDismissExport}>
                    {t('tokenStats:export.close')}
                  </Button>
                </>
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
  facetOptions,
  onChange,
}: {
  filter: AgentLedgerFilters;
  summary: AgentLedgerSummary | null;
  facetOptions?: TokenStatsFacetOptions | null;
  onChange: (patch: Partial<AgentLedgerFilters>) => void;
}) {
  const { t } = useTranslation(['tokenStats']);
  const windows: Array<{ key: '24h' | '7d' | '30d'; label: string }> = [
    { key: '24h', label: t('tokenStats:filters.window.24h') },
    { key: '7d', label: t('tokenStats:filters.window.7d') },
    { key: '30d', label: t('tokenStats:filters.window.30d') },
  ];
  const catalog = facetOptions ?? {
    providers: summary?.byProvider ?? [],
    models: summary?.byModel ?? [],
    projects: summary?.byProject ?? [],
  };
  const providers = chipOptions(catalog.providers, filter.providerIds);
  const models = chipOptions(catalog.models, filter.modelIds);
  const projects = chipOptions(catalog.projects, filter.projectIds);

  return (
    <div className={styles.filterBar} data-testid="token-stats-filters">
      <div
        className={styles.filterRow}
        data-testid="token-stats-filter-row-time"
        role="group"
        aria-label={t('tokenStats:filters.windowLabel')}
      >
        <span className={styles.filterRowLabel}>{t('tokenStats:filters.windowLabel')}</span>
        <div className={styles.filterRowChips}>
          {windows.map((item) => (
            <Button
              key={item.key}
              variant={filter.window === item.key ? 'primary' : 'secondary'}
              size="sm"
              data-testid={`token-stats-window-${item.key}`}
              onClick={() =>
                onChange({ window: item.key, startedAfter: null, startedBefore: null })
              }
            >
              {item.label}
            </Button>
          ))}
          <Button
            variant={filter.window == null ? 'primary' : 'secondary'}
            size="sm"
            data-testid="token-stats-window-custom"
            onClick={() => onChange({ window: null })}
          >
            {t('tokenStats:filters.window.custom')}
          </Button>
        </div>
      </div>
      {filter.window == null ? (
        <div
          className={styles.filterRow}
          data-testid="token-stats-filter-row-custom"
          role="group"
          aria-label={t('tokenStats:filters.customRangeLabel')}
        >
          <span className={styles.filterRowLabel}>{t('tokenStats:filters.customRangeLabel')}</span>
          <div className={styles.filterRowChips}>
            <label className={styles.filterLabel}>
              <span>{t('tokenStats:filters.customStart')}</span>
              <input
                type="datetime-local"
                className={styles.filterSelect}
                data-testid="token-stats-custom-start"
                aria-label={t('tokenStats:filters.customStart')}
                value={datetimeLocalFromRfc3339(filter.startedAfter)}
                onChange={(event) =>
                  onChange({ window: null, startedAfter: rfc3339FromDatetimeLocal(event.target.value) })
                }
              />
            </label>
            <label className={styles.filterLabel}>
              <span>{t('tokenStats:filters.customEnd')}</span>
              <input
                type="datetime-local"
                className={styles.filterSelect}
                data-testid="token-stats-custom-end"
                aria-label={t('tokenStats:filters.customEnd')}
                value={datetimeLocalFromRfc3339(filter.startedBefore)}
                onChange={(event) =>
                  onChange({
                    window: null,
                    startedBefore: rfc3339FromDatetimeLocal(event.target.value),
                  })
                }
              />
            </label>
          </div>
        </div>
      ) : null}
      <ChipFilter
        rowTestId="token-stats-filter-row-provider"
        chipTestIdPrefix="token-stats-provider"
        label={t('tokenStats:filters.providerLabel')}
        allLabel={t('tokenStats:filters.all')}
        emptyLabel={t('tokenStats:filters.emptyOptions')}
        options={providers}
        selected={filter.providerIds}
        onToggle={(id) => onChange({ providerIds: toggleId(filter.providerIds, id) })}
        onClear={() => onChange({ providerIds: null })}
      />
      <ChipFilter
        rowTestId="token-stats-filter-row-model"
        chipTestIdPrefix="token-stats-model"
        label={t('tokenStats:filters.modelLabel')}
        allLabel={t('tokenStats:filters.all')}
        emptyLabel={t('tokenStats:filters.emptyOptions')}
        options={models}
        selected={filter.modelIds}
        onToggle={(id) => onChange({ modelIds: toggleId(filter.modelIds, id) })}
        onClear={() => onChange({ modelIds: null })}
      />
      <ChipFilter
        rowTestId="token-stats-filter-row-project"
        chipTestIdPrefix="token-stats-project"
        label={t('tokenStats:filters.projectLabel')}
        allLabel={t('tokenStats:filters.all')}
        emptyLabel={t('tokenStats:filters.emptyOptions')}
        options={projects}
        selected={filter.projectIds}
        onToggle={(id) => onChange({ projectIds: toggleId(filter.projectIds, id) })}
        onClear={() => onChange({ projectIds: null })}
      />
      <div className={styles.filterRow}>
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
              startedAfter: null,
              startedBefore: null,
            })
          }
        >
          {t('tokenStats:filters.reset')}
        </Button>
      </div>
    </div>
  );
}

function ChipFilter({
  rowTestId,
  chipTestIdPrefix,
  label,
  allLabel,
  emptyLabel,
  options,
  selected,
  onToggle,
  onClear,
}: {
  rowTestId: string;
  chipTestIdPrefix: string;
  label: string;
  allLabel: string;
  emptyLabel: string;
  options: ChipOption[];
  selected: string[] | null | undefined;
  onToggle: (id: string) => void;
  onClear: () => void;
}) {
  const active = selected ?? [];
  const allSelected = active.length === 0;
  return (
    <div className={styles.filterRow} data-testid={rowTestId} role="group" aria-label={label}>
      <span className={styles.filterRowLabel}>{label}</span>
      <div className={styles.filterRowChips}>
        <Button
          variant={allSelected ? 'primary' : 'secondary'}
          size="sm"
          aria-pressed={allSelected}
          data-testid={`${chipTestIdPrefix}-all`}
          onClick={onClear}
        >
          {allLabel}
        </Button>
        {options.length === 0 ? (
          <span className={styles.filterEmpty}>{emptyLabel}</span>
        ) : (
          options.map((option) => {
            const pressed = active.includes(option.id);
            return (
              <Button
                key={option.id}
                variant={pressed ? 'primary' : 'secondary'}
                size="sm"
                aria-pressed={pressed}
                data-testid={`${chipTestIdPrefix}-${option.id}`}
                onClick={() => onToggle(option.id)}
              >
                {option.label}
              </Button>
            );
          })
        )}
      </div>
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

/**
 * Session 明细表。
 *
 * Business Logic（为什么需要）:
 *   明细行展示单条 Agent session 元数据；项目列优先显示后端 LEFT JOIN 得到的
 *   projectName，缺失时回落到 projectId，避免给用户看一串路径编码。
 *
 * Code Logic（做什么）:
 *   - 项目列 `row.projectName ?? row.projectId`；
 *   - 不渲染 outcome 列（已与 Token 统计页 UI 隔离）；
 *   - token 字段统一走 formatNumber 区分 null 与 0。
 */
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
            <td>{row.projectName ?? row.projectId}</td>
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

/**
 * Business Logic（为什么需要）:
 *   三张用量趋势图要共用同一套纵轴单位，避免一张用原始个数、另一张用 k。
 *   用户要求刻度单位固定为百万 token（M），且不要三位小数。
 *
 * Code Logic（做什么）:
 *   把 token 数除以 1e6；最多保留 2 位小数并去掉末尾 0，再加 `M`。
 */
function formatTrendAxisTick(value: number): string {
  if (!Number.isFinite(value) || value < 0) return '0M';
  const millions = value / 1_000_000;
  const text = millions.toFixed(2).replace(/\.?0+$/, '');
  return `${text === '' ? '0' : text}M`;
}

function TrendMetricChart({
  testId,
  title,
  data,
  bucket,
  metric,
  stroke,
}: {
  testId: string;
  title: string;
  data: TrendChartRow[];
  bucket: 'hour' | 'day';
  metric: TrendMetricKey;
  stroke: string;
}) {
  return (
    <section className={styles.trendChart} data-testid={testId} aria-label={title}>
      <h3 className={styles.trendChartTitle}>{title}</h3>
      <ResponsiveContainer width="100%" height={220}>
        <LineChart data={data} margin={{ top: 8, right: 8, bottom: 4, left: 4 }}>
          <CartesianGrid stroke="var(--border-soft)" strokeDasharray="2 4" />
          <XAxis
            dataKey="bucketStart"
            tickFormatter={(value: string) => formatLocalBucketLabel(value, bucket)}
            stroke="var(--fg-muted-readable)"
            tickLine={false}
          />
          <YAxis
            domain={[0, 'auto']}
            allowDecimals={false}
            stroke="var(--fg-muted-readable)"
            tickLine={false}
            width={64}
            tickFormatter={formatTrendAxisTick}
          />
          <Tooltip content={<TrendTooltip bucket={bucket} metric={metric} />} />
          <Line
            type="monotone"
            dataKey={metric}
            name={title}
            stroke={stroke}
            strokeWidth={2}
            dot={{ r: 3, strokeWidth: 0, fill: stroke }}
            activeDot={{ r: 5 }}
          />
        </LineChart>
      </ResponsiveContainer>
    </section>
  );
}

function TrendTooltip({ active, payload, label, bucket, metric }: TrendTooltipProps) {
  const { t } = useTranslation(['tokenStats']);
  if (!active || !payload?.length) return null;
  const point = payload[0]?.payload;
  const timeLabel =
    typeof label === 'string' ? formatLocalBucketTooltip(label, bucket) : String(label ?? '');
  const metricLabel =
    metric === 'input'
      ? t('tokenStats:trend.legend.input')
      : metric === 'output'
        ? t('tokenStats:trend.legend.output')
        : t('tokenStats:trend.legend.cacheRead');
  const value = point?.[metric] ?? 0;
  return (
    <div className={styles.tooltip}>
      <div className={styles.tooltipLabel}>{timeLabel}</div>
      <div className={styles.tooltipRow}>
        <span>{metricLabel}</span>
        <strong>{formatTokenCount(value)}</strong>
      </div>
    </div>
  );
}
