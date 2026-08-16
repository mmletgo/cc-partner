/**
 * ContextMeter —— StatusCard 的上下文使用百分比叶子组件。
 *
 * Business Logic（为什么需要这个组件）:
 *   工作台右侧「当前会话」卡需要把当前 agent session 的 cumulative tokens
 *   （input + cache_read + cache_write）对照 model 的 context_window 给出百分比
 *   与 ProgressBar。拿不到 context_window 时老实显示「无窗口信息」，禁止默认 200K
 *   后假装精确。
 *
 * Code Logic（这个组件做什么）:
 *   - 单行展示 cumulative / window + ProgressBar；
 *   - tone 由 caller 决定（终态 success / 阈值 accent/warn/danger）；
 *   - pct 文本与 ProgressBar label 共存；
 *   - 纯 view，不持有状态，不调用 workbenchApi。
 */
import { useTranslation } from 'react-i18next';

import { ProgressBar, type ProgressBarTone } from '@/components/primitives/ProgressBar';
import { formatTokenCount } from '@/lib/tokenFormat';

import styles from './WorkbenchStatusCardMetrics.module.css';

/**
 * ContextMeter 输入 props。
 *
 * Business Logic: 所有派生数据由 StatusCard 计算后传入；tone 阈值策略由父层决定。
 * Code Logic:
 *   - cumulativeIn：累计 tokens（input + cache_read + cache_write）；null 表示无数据；
 *   - contextWindow：model context window（来自 modelContextWindow 表）；null 表示未识别模型；
 *   - unavailableLabel：「—」/「未提供」等回退文案；
 *   - noWindowLabel：仅在 contextWindow === null 但 cumulativeIn 有值时显示；
 *   - tone：ProgressBar tone（终态 success；中段 accent/warn；>85% danger）。
 */
export interface ContextMeterProps {
  cumulativeIn: number | null;
  contextWindow: number | null;
  unavailableLabel: string;
  noWindowLabel: string;
  tone: ProgressBarTone;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   让 StatusCard 的「上下文长度 / 使用百分比」拥有独立叶子，便于调整样式与扩展
 *   （例如未来加 cache breakdown）而不影响 StatusCard 主结构。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 cumulative / window 文本 + ProgressBar；contextWindow 缺失时只显示
 *   cumulative + noWindowLabel，无 ProgressBar。
 */
export function ContextMeter({
  cumulativeIn,
  contextWindow,
  unavailableLabel,
  noWindowLabel,
  tone,
}: ContextMeterProps) {
  const { t } = useTranslation(['workbench']);
  const contextLabel = t('workbench:metricsContext');

  const cumulativeText = formatTokenCount(cumulativeIn) ?? unavailableLabel;
  const windowText = formatTokenCount(contextWindow);
  const pct =
    contextWindow != null && contextWindow > 0 && cumulativeIn != null
      ? Math.min(1, Math.max(0, cumulativeIn / contextWindow))
      : null;

  return (
    <div className={styles.contextMeter} data-testid="workbench-status-context-meter">
      <div className={styles.contextHeaderRow}>
        <dt>{contextLabel}</dt>
        <dd className={styles.contextValue}>
          {cumulativeText}
          {windowText != null ? (
            <>
              <span className={styles.contextDivider}>/</span>
              <span>{windowText}</span>
            </>
          ) : cumulativeIn != null ? (
            <span className={styles.contextHint}> · {noWindowLabel}</span>
          ) : null}
        </dd>
      </div>
      {pct !== null ? (
        <ProgressBar
          value={pct}
          tone={tone}
          size="sm"
          aria-label={t('workbench:metricsContextAria', { pct: Math.round(pct * 100) })}
        >
          <span className={styles.contextPct}>{Math.round(pct * 100)}%</span>
        </ProgressBar>
      ) : null}
    </div>
  );
}