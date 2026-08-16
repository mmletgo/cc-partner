/**
 * TokenRateRow —— StatusCard 的输入/输出速率叶子组件。
 *
 * Business Logic（为什么需要这个组件）:
 *   工作台右侧「当前会话」卡展示当前 agent session 的平均 input / output tok/s。
 *   无 live usage 且无 ledger 时显示「未提供」，禁止假装数字避免误导。
 *
 * Code Logic（这个组件做什么）:
 *   - 单行两列（In / Out）；
 *   - 数值走 formatTokenRate（>1000 → k、>=1M → M）；
 *   - null/0/非有限数走 unavailableLabel；
 *   - 纯 view，不持有状态，不调用 workbenchApi。
 */
import { useTranslation } from 'react-i18next';

import { formatTokenRate } from '@/lib/tokenFormat';

import styles from './WorkbenchStatusCardMetrics.module.css';

/**
 * TokenRateRow 输入 props。
 *
 * Business Logic: 所有数值由 StatusCard 计算后传入；组件本身只负责展示与 i18n。
 * Code Logic:
 *   - speedInTps / speedOutTps：null 表示无 live usage 且无 ledger（或速率无法计算）；
 *   - unavailableLabel：「—」/「未提供」等回退文案；
 */
export interface TokenRateRowProps {
  speedInTps: number | null;
  speedOutTps: number | null;
  unavailableLabel: string;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   让状态卡的 4 个 agent 指标中的两个（输入速率 / 输出速率）拥有独立叶子，
 *   避免 StatusCard.tsx 行数膨胀；后续调整样式不影响 StatusCard 主结构。
 *
 * Code Logic（这个组件做什么）:
 *   渲染两列 dt/dd；数值走 formatTokenRate；null 走 unavailableLabel。
 */
export function TokenRateRow({ speedInTps, speedOutTps, unavailableLabel }: TokenRateRowProps) {
  const { t } = useTranslation(['workbench']);
  const inLabel = t('workbench:metricsIn');
  const outLabel = t('workbench:metricsOut');
  const inValue = formatTokenRate(speedInTps) ?? unavailableLabel;
  const outValue = formatTokenRate(speedOutTps) ?? unavailableLabel;

  return (
    <div className={styles.metricsRow} data-testid="workbench-status-token-rate-row">
      <div className={styles.metricCell}>
        <dt>{inLabel}</dt>
        <dd data-state={speedInTps == null ? 'unavailable' : 'available'}>{inValue}</dd>
      </div>
      <div className={styles.metricCell}>
        <dt>{outLabel}</dt>
        <dd data-state={speedOutTps == null ? 'unavailable' : 'available'}>{outValue}</dd>
      </div>
    </div>
  );
}