/**
 * BatteryWorkbenchScrim — 工作台余额耗尽时盖住 main。
 *
 * Business Logic（为什么需要这个组件）:
 *   充电模式剩余 0 时拦工作台交互，但不挡侧栏电池环、toast 和 Inbox。
 *
 * Code Logic（这个组件做什么）:
 *   仅 /workbench 且 charging && remainingMs<=0 时绝对定位盖住 main；
 *   提供去健康 / 打开记单词。
 */

import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';

import { Button } from '@/components/primitives';
import styles from './BatteryWorkbenchScrim.module.css';

export interface BatteryWorkbenchScrimProps {
  visible: boolean;
  onOpenGame: () => void;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   耗尽后用户必须能从遮罩走到健康或游戏去充电。
 *
 * Code Logic（这个组件做什么）:
 *   见文件头。
 */
export function BatteryWorkbenchScrim({
  visible,
  onOpenGame,
}: BatteryWorkbenchScrimProps): ReactElement | null {
  const { t } = useTranslation('battery');
  if (!visible) return null;
  return (
    <div className={styles.scrim} data-testid="battery-workbench-scrim" role="dialog" aria-modal="true">
      <div className={styles.card}>
        <h2 className={styles.title}>{t('scrim.title')}</h2>
        <p className={styles.body}>{t('scrim.body')}</p>
        <div className={styles.actions}>
          <Link to="/health" className={styles.linkReset}>
            <Button variant="primary" size="sm">
              {t('scrim.goHealth')}
            </Button>
          </Link>
          <Button variant="secondary" size="sm" onClick={onOpenGame}>
            {t('scrim.openGame')}
          </Button>
        </div>
      </div>
    </div>
  );
}
