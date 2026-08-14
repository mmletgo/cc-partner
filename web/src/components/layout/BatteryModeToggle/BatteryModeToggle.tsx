/**
 * BatteryModeToggle（充电 / 无限切换）
 *
 * Business Logic（为什么需要这个组件）:
 *   自我约束入口必须放在主题按钮前面，图标表示目标态；充电时画余额环。
 *
 * Code Logic（这个组件做什么）:
 *   charging 显示 ∞（切到无限）；unlimited 显示电池（切到充电）；
 *   环满 = maxBalanceMs；<5 分 warn，0 danger。文案全部 t()。
 */

import { useCallback, type ReactElement } from 'react';
import { useTranslation } from 'react-i18next';

import { BatteryIcon, InfinityIcon } from '@/lib/icons';
import { formatBatteryTime, remainingMinutesFromMs } from '@/lib/batteryTime';
import { BATTERY_WARN_MINUTES, type BatterySnapshot } from '@/lib/types/battery';
import styles from './BatteryModeToggle.module.css';

export interface BatteryModeToggleProps {
  snapshot: BatterySnapshot | null;
  onToggle: (next: 'charging' | 'unlimited') => void;
  className?: string;
}

const RING_R = 9;
const RING_C = 2 * Math.PI * RING_R;

/**
 * Business Logic（为什么需要这个组件）:
 *   footer 需要一眼看出模式与剩余，并一键切回无限。
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
  const title = charging
    ? t('titleCharging', { time: timeLabel })
    : t('titleUnlimited', { time: timeLabel });
  const aria = charging ? t('toggleToUnlimited') : t('toggleToCharging');
  const toneClass =
    charging && minutes <= 0
      ? styles.toggleDanger
      : charging && minutes < BATTERY_WARN_MINUTES
        ? styles.toggleWarn
        : '';
  const cls = [styles.toggle, toneClass, className].filter(Boolean).join(' ');

  /**
   * Business Logic（为什么需要这个函数）:
   *   点一下切到目标态，不抹余额。
   *
   * Code Logic（这个函数做什么）:
   *   charging → unlimited；unlimited → charging。
   */
  const handleClick = useCallback((): void => {
    onToggle(charging ? 'unlimited' : 'charging');
  }, [charging, onToggle]);

  return (
    <button
      type="button"
      className={cls}
      onClick={handleClick}
      aria-label={aria}
      title={title}
      data-testid="battery-mode-toggle"
      data-mode={charging ? 'charging' : 'unlimited'}
    >
      {charging ? (
        <svg className={styles.ring} viewBox="0 0 22 22" aria-hidden="true">
          <circle className={styles.ringTrack} cx="11" cy="11" r={RING_R} />
          <circle
            className={styles.ringValue}
            cx="11"
            cy="11"
            r={RING_R}
            strokeDasharray={RING_C}
            strokeDashoffset={RING_C * (1 - ratio)}
          />
        </svg>
      ) : null}
      <span className={styles.icon}>
        {charging ? <InfinityIcon size={14} /> : <BatteryIcon size={14} />}
      </span>
    </button>
  );
}
