/**
 * StatsChart - 健康提醒活动统计图表
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要直观看到「今天在哪些 app / 窗口上花了最多时间」和「一天 24 小时活跃分布」，
 *   以了解自己的屏幕使用习惯。用 recharts 把后端 get_activity_detail 的数据可视化：
 *   左侧 app 使用时长排行 top8、下方窗口标题排行 top8（横向柱状图，倒序最长的在最上），
 *   右侧 24 小时活跃分布（纵向柱状图）。无数据时显示占位文案。
 *
 * Code Logic（这个组件做什么）:
 *   纯展示组件，接收 ActivityDetail prop。appData / windowData 取前 8 项；
 *   hourData 把 24 元素数组映射成 {h: 小时字符串, mins} 供 XAxis dataKey="h"。
 *   用 ResponsiveContainer 自适应宽度；layout="vertical" 实现横向柱（XAxis number /
 *   YAxis category）。hooks 仅 useTranslation，无 early return 故无顺序约束。
 */
import { useTranslation } from 'react-i18next';
import { Bar, BarChart, XAxis, YAxis, Tooltip, ResponsiveContainer } from 'recharts';
import type { TooltipContentProps } from 'recharts';
import type { ActivityDetail, AppUsageItem } from '@/lib/types';
import styles from './StatsChart.module.css';

interface StatsChartProps {
  /** 活动明细（app 排行 + 窗口标题排行 + 24 小时分布），来自 get_activity_detail */
  detail: ActivityDetail;
}

interface ChartTooltipProps extends TooltipContentProps {
  /** tooltip 数值单位 */
  unit: string;
}

interface RankingPanelProps {
  /** 排行图标题 */
  title: string;
  /** 排行图说明 */
  caption: string;
  /** 排行数据，已截到 top N */
  data: Array<{ name: string; minutes: number }>;
  /** 空态文案 */
  empty: string;
  /** tooltip 分钟单位 */
  unit: string;
  /** 柱系列名 */
  seriesName: string;
  /** 分类轴宽度，窗口标题比进程名更长 */
  yAxisWidth: number;
  /** 是否占满整行（窗口标题排行） */
  wide?: boolean;
}

/**
 * 截断横向柱状图分类轴上的长窗口标题，避免挤掉柱体。
 *
 * Business Logic（为什么需要这个函数）:
 *   窗口标题常含文件路径或网页标题，原样画在 Y 轴会把图表挤没。
 *
 * Code Logic（这个函数做什么）:
 *   超过 max 个字符时截断并加省略号；tooltip 仍用完整 name。
 */
function truncateLabel(value: unknown, max = 28): string {
  const text = String(value ?? '');
  if (text.length <= max) return text;
  return `${text.slice(0, max - 1)}…`;
}

/**
 * 渲染健康统计图表 tooltip
 *
 * Business Logic（为什么需要这个函数）:
 *   默认 Recharts tooltip 视觉与项目设计系统不一致,且没有统一展示分钟单位。
 *
 * Code Logic（这个函数做什么）:
 *   接收 Recharts tooltip props,在 active 且有 payload 时渲染 token 化 tooltip;
 *   非激活状态返回 null。
 */
function ChartTooltip(props: ChartTooltipProps) {
  const { active, payload, label, unit } = props;

  if (!active || !payload?.length) return null;

  return (
    <div className={styles.tooltip}>
      <div className={styles.tooltipLabel}>{label}</div>
      <div className={styles.tooltipRow}>
        <span>{payload[0]?.name}</span>
        <strong>{Number(payload[0]?.value ?? 0)} {unit}</strong>
      </div>
    </div>
  );
}

/**
 * 渲染一条横向排行柱状图（app / 窗口标题共用）。
 *
 * Business Logic（为什么需要这个函数）:
 *   app 与窗口标题是同一类「按分钟倒序」统计，视觉和交互应一致，避免复制两套图。
 *
 * Code Logic（这个函数做什么）:
 *   空数据画占位；否则画 vertical BarChart，Y 轴按 yAxisWidth 截断长标签。
 */
function RankingPanel({
  title,
  caption,
  data,
  empty,
  unit,
  seriesName,
  yAxisWidth,
  wide = false,
}: RankingPanelProps) {
  return (
    <section className={wide ? `${styles.panel} ${styles.wide}` : styles.panel}>
      <div className={styles.panelHeader}>
        <h3 className={styles.title}>{title}</h3>
        <p className={styles.caption}>{caption}</p>
      </div>
      {data.length === 0 ? (
        <p className={styles.empty}>{empty}</p>
      ) : (
        <>
          <ul className="sr-only">
            {data.map((item) => (
              <li key={item.name}>
                {item.name} {item.minutes} {unit}
              </li>
            ))}
          </ul>
          <ResponsiveContainer width="100%" height={wide ? 280 : 240}>
            <BarChart data={data} layout="vertical" margin={{ top: 8, right: 8, bottom: 8, left: 8 }}>
              <XAxis type="number" stroke="var(--meta)" tick={{ fill: 'var(--muted)', fontSize: 12 }} />
              <YAxis
                type="category"
                dataKey="name"
                width={yAxisWidth}
                stroke="var(--meta)"
                tick={{ fill: 'var(--muted)', fontSize: 12 }}
                tickFormatter={(value) => truncateLabel(value, wide ? 36 : 18)}
              />
              <Tooltip
                content={(props) => <ChartTooltip {...props} unit={unit} />}
                cursor={{ fill: 'var(--accent-soft)' }}
              />
              <Bar dataKey="minutes" name={seriesName} fill="var(--success)" radius={[0, 6, 6, 0]} isAnimationActive={false} />
            </BarChart>
          </ResponsiveContainer>
        </>
      )}
    </section>
  );
}

/**
 * 把排行项收成图表数据。
 *
 * Business Logic（为什么需要这个函数）:
 *   app / 窗口标题都只展示 top8，映射逻辑必须一致。
 *
 * Code Logic（这个函数做什么）:
 *   取前 8 项并抽出 name/minutes。
 */
function toRankingData(items: AppUsageItem[] | undefined): Array<{ name: string; minutes: number }> {
  return (items ?? []).slice(0, 8).map((item) => ({ name: item.name, minutes: item.minutes }));
}

/**
 * StatsChart 组件：渲染 app 排行 + 窗口标题排行 + 24 小时活跃分布。
 */
export function StatsChart({ detail }: StatsChartProps) {
  const { t } = useTranslation(['health', 'common']);
  const appData = toRankingData(detail.appUsage);
  const windowData = toRankingData(detail.windowUsage);
  const hourData = detail.hourly.map((mins, h) => ({ h: `${h}`, mins }));
  const minuteUnit = t('health:minutesUnit');
  const seriesName = t('health:activeToday');
  const empty = t('health:noData');

  return (
    <div className={styles.grid}>
      <RankingPanel
        title={t('health:appUsageTitle')}
        caption={t('health:topAppsCaption')}
        data={appData}
        empty={empty}
        unit={minuteUnit}
        seriesName={seriesName}
        yAxisWidth={112}
      />

      <section className={styles.panel}>
        <div className={styles.panelHeader}>
          <h3 className={styles.title}>{t('health:hourlyTitle')}</h3>
          <p className={styles.caption}>{t('health:hourlyCaption')}</p>
        </div>
        <ResponsiveContainer width="100%" height={240}>
          <BarChart data={hourData} margin={{ top: 8, right: 8, bottom: 8, left: 0 }}>
            <XAxis dataKey="h" stroke="var(--meta)" tick={{ fill: 'var(--muted)', fontSize: 12 }} />
            <YAxis stroke="var(--meta)" tick={{ fill: 'var(--muted)', fontSize: 12 }} />
            <Tooltip
              content={(props) => <ChartTooltip {...props} unit={minuteUnit} />}
              cursor={{ fill: 'var(--accent-soft)' }}
            />
            <Bar dataKey="mins" name={seriesName} fill="var(--accent)" radius={[6, 6, 0, 0]} isAnimationActive={false} />
          </BarChart>
        </ResponsiveContainer>
      </section>

      <RankingPanel
        title={t('health:windowUsageTitle')}
        caption={t('health:topWindowsCaption')}
        data={windowData}
        empty={empty}
        unit={minuteUnit}
        seriesName={seriesName}
        yAxisWidth={220}
        wide
      />
    </div>
  );
}

StatsChart.displayName = 'StatsChart';
