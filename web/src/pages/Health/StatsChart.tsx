/**
 * StatsChart - 健康提醒活动统计图表
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要直观看到「今天在哪些 app 上花了最多时间」和「一天 24 小时活跃分布」，
 *   以了解自己的屏幕使用习惯。用 recharts 把后端 get_activity_detail 的数据可视化：
 *   左侧 app 使用时长排行 top8（横向柱状图，倒序最长的在最上），右侧 24 小时活跃
 *   分布（纵向柱状图）。无数据时显示占位文案。
 *
 * Code Logic（这个组件做什么）:
 *   纯展示组件，接收 ActivityDetail prop。appData 取 appUsage 前 8 项；
 *   hourData 把 24 元素数组映射成 {h: 小时字符串, mins} 供 XAxis dataKey="h"。
 *   用 ResponsiveContainer 自适应宽度；layout="vertical" 实现横向柱（XAxis number /
 *   YAxis category）。hooks 仅 useTranslation，无 early return 故无顺序约束。
 */
import { useTranslation } from 'react-i18next';
import { Bar, BarChart, XAxis, YAxis, Tooltip, ResponsiveContainer } from 'recharts';
import type { ActivityDetail } from '@/lib/types';

interface StatsChartProps {
  /** 活动明细（app 排行 + 24 小时分布），来自 get_activity_detail */
  detail: ActivityDetail;
}

/**
 * StatsChart 组件：渲染 app 使用时长排行 + 24 小时活跃分布两个图表。
 */
export function StatsChart({ detail }: StatsChartProps) {
  const { t } = useTranslation(['health', 'common']);
  const appData = detail.appUsage.slice(0, 8).map((a) => ({ name: a.name, minutes: a.minutes }));
  const hourData = detail.hourly.map((mins, h) => ({ h: `${h}`, mins }));

  return (
    <div style={{ display: 'flex', gap: 24, flexWrap: 'wrap' }}>
      <div style={{ flex: '1 1 320px' }}>
        <h4>{t('health:appUsageTitle')}</h4>
        {appData.length === 0 ? (
          <p>{t('health:noData')}</p>
        ) : (
          <ResponsiveContainer width="100%" height={240}>
            <BarChart data={appData} layout="vertical" margin={{ left: 20 }}>
              <XAxis type="number" />
              <YAxis type="category" dataKey="name" width={100} />
              <Tooltip />
              <Bar dataKey="minutes" fill="#34C759" />
            </BarChart>
          </ResponsiveContainer>
        )}
      </div>
      <div style={{ flex: '1 1 320px' }}>
        <h4>{t('health:hourlyTitle')}</h4>
        <ResponsiveContainer width="100%" height={240}>
          <BarChart data={hourData}>
            <XAxis dataKey="h" />
            <YAxis />
            <Tooltip />
            <Bar dataKey="mins" fill="#007AFF" />
          </BarChart>
        </ResponsiveContainer>
      </div>
    </div>
  );
}

StatsChart.displayName = 'StatsChart';
