/**
 * SessionQualityRow —— StatusCard 的首 token 平均耗时 + 缓存命中率。
 *
 * Business Logic（为什么需要这个组件）:
 *   工作台右侧「当前会话」卡需要两项质量指标：
 *   用户发出指令到首条助手回复的平均等待，以及 cache_read / (cache_read + input)。
 *
 * Code Logic（这个组件做什么）:
 *   - 单行两列，复用 metricsRow；
 *   - 时长走 formatFirstTokenLatency，命中率走 formatCacheHitRate；
 *   - null 走 unavailableLabel。
 */
import { useTranslation } from 'react-i18next';

import { formatCacheHitRate, formatFirstTokenLatency } from '@/lib/tokenFormat';

import styles from './WorkbenchStatusCardMetrics.module.css';

/**
 * SessionQualityRow 输入 props。
 *
 * Business Logic: 数值由 StatusCard 计算后传入；组件只负责展示。
 */
export interface SessionQualityRowProps {
  firstTokenAvgMs: number | null;
  cacheHitRate: number | null;
  unavailableLabel: string;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   与 TokenRateRow 并列，避免把新指标塞进速率行或元信息 grid。
 *
 * Code Logic（这个组件做什么）:
 *   渲染两列 dt/dd。
 */
export function SessionQualityRow({
  firstTokenAvgMs,
  cacheHitRate,
  unavailableLabel,
}: SessionQualityRowProps) {
  const { t } = useTranslation(['workbench']);
  const firstLabel = t('workbench:metricsFirstToken');
  const hitLabel = t('workbench:metricsCacheHit');
  const firstValue = formatFirstTokenLatency(firstTokenAvgMs) ?? unavailableLabel;
  const hitValue = formatCacheHitRate(cacheHitRate) ?? unavailableLabel;

  return (
    <div className={styles.metricsRow} data-testid="workbench-status-session-quality-row">
      <div className={styles.metricCell}>
        <dt>{firstLabel}</dt>
        <dd data-state={firstTokenAvgMs == null ? 'unavailable' : 'available'}>{firstValue}</dd>
      </div>
      <div className={styles.metricCell}>
        <dt>{hitLabel}</dt>
        <dd data-state={cacheHitRate == null ? 'unavailable' : 'available'}>{hitValue}</dd>
      </div>
    </div>
  );
}
