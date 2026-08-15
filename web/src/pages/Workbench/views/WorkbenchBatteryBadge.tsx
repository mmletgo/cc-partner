/**
 * WorkbenchBatteryBadge — 工作台标题旁电池模式徽标。
 *
 * Business Logic（为什么需要这个组件）:
 *   用户要在工作台标题旁一眼分辨当前自我约束模式：无限模式显示 ∞ 标识；
 *   充电模式显示剩余时长与余额比例微型进度条，低电量用 warn/danger 提示，
 *   替代原先无信息量的 Terminal sessions 徽标。
 *
 * Code Logic（这个组件做什么）:
 *   自挂 useBattery()，snapshot 为 null（加载中）时渲染 null。
 *   unlimited 渲染 InfinityIcon 胶囊（role=img）；
 *   charging 渲染 progressbar 微型条 + 时长文本：
 *   ratio = clamp01(remainingMs / maxBalanceMs)（maxBalanceMs<=0 按 0），
 *   percent = round(ratio * 100)，tone 按剩余分钟 <=0 danger /
 *   <BATTERY_WARN_MINUTES warn / 否则 accent。
 *   不复用 primitives/ProgressBar：其 wrapper 合同（width:100% + 固定 label 间距）
 *   面向整行进度展示，标题旁需要固定宽度的内联微型条，故本组件自带 .track/.fill。
 */

import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';

import { useBattery } from '@/hooks/useBattery';
import { formatBatteryTime, remainingMinutesFromMs } from '@/lib/batteryTime';
import { InfinityIcon } from '@/lib/icons';
import { BATTERY_WARN_MINUTES } from '@/lib/types/battery';
import styles from './WorkbenchBatteryBadge.module.css';

/**
 * Business Logic（为什么需要这个函数）:
 *   余额比例必须落在 0..1，负数或超上限都不能撑破微型进度条。
 *
 * Code Logic（这个函数做什么）:
 *   把 value 钳制到 [0, 1] 区间返回。
 */
function clamp01(value: number): number {
  return Math.max(0, Math.min(1, value));
}

/**
 * Business Logic（为什么需要这个组件）:
 *   工作台标题旁的 Terminal sessions 徽标替换为电池模式徽标，
 *   与 footer 切换器共用同一份 useBattery 快照。
 *
 * Code Logic（这个组件做什么）:
 *   见文件头。
 */
export function WorkbenchBatteryBadge(): ReactElement | null {
  const { t } = useTranslation('battery');
  const { snapshot } = useBattery();
  if (!snapshot) return null;

  const timeLabel = formatBatteryTime(snapshot.remainingMs, t);

  if (snapshot.mode === 'unlimited') {
    return (
      <span
        className={styles.badge}
        data-mode="unlimited"
        role="img"
        aria-label={t('modeUnlimited')}
        title={t('titleUnlimited', { time: timeLabel })}
        data-testid="workbench-battery-badge"
      >
        <InfinityIcon size={14} />
      </span>
    );
  }

  const ratio =
    snapshot.maxBalanceMs > 0
      ? clamp01(snapshot.remainingMs / snapshot.maxBalanceMs)
      : 0;
  const percent = Math.round(ratio * 100);
  const minutes = remainingMinutesFromMs(snapshot.remainingMs);
  // 低电量警示顺序：<=0 分 danger 优先，其次 <BATTERY_WARN_MINUTES 分 warn
  const tone = minutes <= 0 ? 'danger' : minutes < BATTERY_WARN_MINUTES ? 'warn' : 'accent';

  return (
    <span
      className={styles.badge}
      data-mode="charging"
      data-tone={tone}
      title={t('titleCharging', { time: timeLabel })}
      data-testid="workbench-battery-badge"
    >
      <span
        className={styles.track}
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={percent}
        aria-label={t('modeCharging')}
      >
        <span className={styles.fill} style={{ width: `${percent}%` }} />
      </span>
      <span className={styles.time}>{timeLabel}</span>
    </span>
  );
}
