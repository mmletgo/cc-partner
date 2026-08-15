/**
 * BatteryModeToggle（充电 / 无限两段式切换器）
 *
 * Business Logic（为什么需要这个组件）:
 *   footer 需要一眼分辨当前自我约束模式并一键切换；
 *   充电模式的剩余余额直接由电池图标的填充比例表达，低电量给出 warn/danger 警示。
 *
 * Code Logic（这个组件做什么）:
 *   胶囊容器内两个 aria-pressed 按钮（范式对齐 LanguageSwitcher）：
 *   充电档渲染 BatteryLevelIcon（fill = remainingMs/maxBalanceMs，clamp 0..1），
 *   无限档渲染 ∞；当前模式高亮 accent；
 *   charging 且剩余 <=0 分 danger、<BATTERY_WARN_MINUTES 分 warn。
 *   文案全部 t()。
 */

import { useCallback, type ReactElement } from 'react';
import { useTranslation } from 'react-i18next';

import { BatteryLevelIcon, InfinityIcon } from '@/lib/icons';
import { formatBatteryTime, remainingMinutesFromMs } from '@/lib/batteryTime';
import { BATTERY_WARN_MINUTES, type BatterySnapshot } from '@/lib/types/battery';
import styles from './BatteryModeToggle.module.css';

export interface BatteryModeToggleProps {
  snapshot: BatterySnapshot | null;
  onToggle: (next: 'charging' | 'unlimited') => void;
  className?: string;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   footer 需要一眼看出当前模式与剩余电量，并在充电/无限间一键切换。
 *
 * Code Logic（这个组件做什么）:
 *   见文件头。
 */
export function BatteryModeToggle({
  snapshot,
  onToggle,
  className,
}: BatteryModeToggleProps): ReactElement {
  const { t } = useTranslation('battery');
  const charging = snapshot?.mode !== 'unlimited';
  const remainingMs = snapshot?.remainingMs ?? 0;
  const maxMs = snapshot?.maxBalanceMs && snapshot.maxBalanceMs > 0 ? snapshot.maxBalanceMs : 1;
  const minutes = remainingMinutesFromMs(remainingMs);
  const ratio = Math.max(0, Math.min(1, remainingMs / maxMs));
  const timeLabel = formatBatteryTime(remainingMs, t);
  // 低电量警示仅对充电档有意义：<=0 danger，<BATTERY_WARN_MINUTES warn
  const lowTone =
    charging && minutes <= 0
      ? styles.optionDanger
      : charging && minutes < BATTERY_WARN_MINUTES
        ? styles.optionWarn
        : '';
  const cls = [styles.switcher, className].filter(Boolean).join(' ');
  const chargingOptionCls = [
    charging ? styles.optionActive : styles.option,
    lowTone,
  ].filter(Boolean).join(' ');

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点充电档即希望回到充电模式。
   *
   * Code Logic（这个函数做什么）:
   *   固定回调 onToggle('charging')。
   */
  const handleChargingClick = useCallback((): void => {
    onToggle('charging');
  }, [onToggle]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点无限档即希望解除自我约束。
   *
   * Code Logic（这个函数做什么）:
   *   固定回调 onToggle('unlimited')。
   */
  const handleUnlimitedClick = useCallback((): void => {
    onToggle('unlimited');
  }, [onToggle]);

  return (
    <div
      role="group"
      aria-label={t('groupLabel')}
      className={cls}
      data-testid="battery-mode-toggle"
      data-mode={charging ? 'charging' : 'unlimited'}
    >
      <button
        type="button"
        className={chargingOptionCls}
        onClick={handleChargingClick}
        aria-pressed={charging}
        aria-label={t('modeCharging')}
        title={t('titleCharging', { time: timeLabel })}
      >
        <BatteryLevelIcon size={14} level={ratio} />
      </button>
      <button
        type="button"
        className={!charging ? styles.optionActive : styles.option}
        onClick={handleUnlimitedClick}
        aria-pressed={!charging}
        aria-label={t('modeUnlimited')}
        title={t('titleUnlimited', { time: timeLabel })}
      >
        <InfinityIcon size={14} />
      </button>
    </div>
  );
}
