/**
 * WaterToast - 喝水提醒应用内 toast
 *
 * Business Logic（为什么需要这个组件）:
 *   后端 health daemon 在喝水提醒时点 emit `health:water`。用户需要一个应用内悬浮
 *   toast 提醒补水并提供「已喝水」按钮记录一次喝水(后端 record_water)。常驻渲染于
 *   主窗口(AppShell 内),非 overlay 路由,确保任意页面下都能弹出。
 *
 * Code Logic（这个组件做什么）:
 *   - listen `health:water` → setVisible(true)
 *   - 「已喝水」按钮调 healthApi.recordWater() 后关闭
 *   - hooks(useTranslation/useState/useEffect)全部在 early return 之前(项目规则 20)
 */
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { listen } from '@tauri-apps/api/event';
import { Button } from '@/components/primitives';
import { healthApi } from '@/api/health';
import styles from './HealthToast.module.css';

/**
 * 渲染喝水提醒 toast(可见时显示,否则返回 null)
 *
 * @returns 固定定位悬浮卡 或 null
 */
export default function WaterToast() {
  const { t } = useTranslation(['health', 'common']);
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    const unlisten = listen('health:water', () => setVisible(true));
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  if (!visible) return null;

  /** 记录一次喝水后关闭 toast */
  const drank = async () => {
    await healthApi.recordWater();
    setVisible(false);
  };

  return (
    <div
      className={`${styles.toast} ${styles.toastWater}`}
      role="alert"
      aria-live="assertive"
    >
      <div className={styles.title}>💧 {t('health:waterTitle')}</div>
      <p className={styles.body}>{t('health:waterBody')}</p>
      <div className={styles.actions}>
        <Button variant="primary" size="sm" onClick={drank}>
          {t('health:drank')}
        </Button>
      </div>
    </div>
  );
}

WaterToast.displayName = 'WaterToast';
