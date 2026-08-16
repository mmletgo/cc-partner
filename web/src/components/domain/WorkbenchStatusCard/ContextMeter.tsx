/**
 * ContextMeter —— StatusCard 的上下文占用 + 模型最大上下文叶子组件。
 *
 * Business Logic（为什么需要这个组件）:
 *   工作台右侧「当前会话」卡需要按 ccstatusline-zh 口径展示：
 *   - 上下文用量 = 当前窗口占用（末轮 input + cache_read + cache_write），单位以 k 为主；
 *   - 上下文长度 = 模型最大上下文（provider 上报 / `[1M]` hint / 已知表 / 非空未知 200k）；
 *
 * Code Logic（这个组件做什么）:
 *   - 两行：用量 + 长度；有占用且有窗口时渲染 ProgressBar；
 *   - 用量用 1 位小数 k，窗口用整数 k（与 ccstatusline ContextLength / ContextWindow 一致）；
 *   - tone 由 caller 决定；纯 view，不持有状态。
 */
import { useTranslation } from 'react-i18next';

import { ProgressBar, type ProgressBarTone } from '@/components/primitives/ProgressBar';
import { formatContextTokens } from '@/lib/tokenFormat';

import styles from './WorkbenchStatusCardMetrics.module.css';

/**
 * ContextMeter 输入 props。
 *
 * Business Logic: 所有派生数据由 StatusCard 计算后传入；tone 阈值策略由父层决定。
 * Code Logic:
 *   - contextUsed：当前占用；null 表示无 occupancy 数据；
 *   - contextWindow：model max context；null 表示未识别模型且 provider 未上报；
 *   - unavailableLabel：「未提供」回退文案；
 *   - noWindowLabel：窗口未知时的长度行文案；
 *   - tone：ProgressBar tone。
 */
export interface ContextMeterProps {
  contextUsed: number | null;
  contextWindow: number | null;
  unavailableLabel: string;
  noWindowLabel: string;
  tone: ProgressBarTone;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   让「上下文用量」与「上下文长度」分开展示，避免用户把分母当成唯一窗口信息。
 *
 * Code Logic（这个组件做什么）:
 *   渲染用量行、可选 ProgressBar、长度行。
 */
export function ContextMeter({
  contextUsed,
  contextWindow,
  unavailableLabel,
  noWindowLabel,
  tone,
}: ContextMeterProps) {
  const { t } = useTranslation(['workbench']);
  const usedLabel = t('workbench:metricsContext');
  const lengthLabel = t('workbench:metricsContextLength');

  const usedText = formatContextTokens(contextUsed, 1) ?? unavailableLabel;
  const windowText = formatContextTokens(contextWindow, 0);
  const pct =
    contextWindow != null && contextWindow > 0 && contextUsed != null
      ? Math.min(1, Math.max(0, contextUsed / contextWindow))
      : null;

  return (
    <div className={styles.contextMeter} data-testid="workbench-status-context-meter">
      <div className={styles.contextHeaderRow}>
        <dt>{usedLabel}</dt>
        <dd className={styles.contextValue} data-state={contextUsed == null ? 'unavailable' : undefined}>
          {usedText}
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
      <div className={styles.contextHeaderRow}>
        <dt>{lengthLabel}</dt>
        <dd
          className={styles.contextValue}
          data-state={windowText == null ? 'unavailable' : undefined}
        >
          {windowText ?? noWindowLabel}
        </dd>
      </div>
    </div>
  );
}
