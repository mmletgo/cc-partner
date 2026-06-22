/**
 * ReminderToast - 久坐提醒应用内 toast
 *
 * Business Logic（为什么需要这个组件）:
 *   后端 health daemon 判定连续工作达阈值(且未贪睡/不在免打扰/notify_enabled)时
 *   emit `health:reminder`(载荷 {workWindowSeconds})。除已有的系统通知外,用户还
 *   需要一个应用内悬浮 toast 直接操作:推迟 5/10 分钟贪睡、跳过本次。常驻渲染于
 *   主窗口(AppShell 内),非 overlay 路由,确保任意页面下都能弹出。
 *
 * Code Logic（这个组件做什么）:
 *   - listen `health:reminder` → setVisible(true)
 *   - 推迟按钮调 healthApi.snooze(minutes) 后关闭
 *   - 跳过按钮调 healthApi.skip() 后关闭
 *   - hooks(useTranslation/useState/useEffect)全部在 early return 之前(项目规则 20)
 */
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { listen } from '@tauri-apps/api/event';
import { Button } from '@/components/primitives';
import { healthApi } from '@/api/health';
import styles from './HealthToast.module.css';

/**
 * 渲染久坐提醒 toast(可见时显示,否则返回 null)
 *
 * @returns 固定定位悬浮卡 或 null
 */
export default function ReminderToast() {
  const { t } = useTranslation(['health', 'common']);
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    const unlisten = listen('health:reminder', () => setVisible(true));
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  if (!visible) return null;

  /** 推迟 N 分钟后关闭 toast */
  const snooze = async (minutes: number) => {
    await healthApi.snooze(minutes);
    setVisible(false);
  };

  /** 跳过本次提醒后关闭 toast */
  const skip = async () => {
    await healthApi.skip();
    setVisible(false);
  };

  return (
    <div className={styles.toast} role="alert" aria-live="assertive">
      <div className={styles.title}>{t('health:reminderTitle')}</div>
      <p className={styles.body}>{t('health:reminderBody')}</p>
      <div className={styles.actions}>
        <Button variant="secondary" size="sm" onClick={() => snooze(5)}>
          {t('health:snooze5')}
        </Button>
        <Button variant="secondary" size="sm" onClick={() => snooze(10)}>
          {t('health:snooze10')}
        </Button>
        <Button variant="ghost" size="sm" onClick={skip}>
          {t('health:skip')}
        </Button>
      </div>
    </div>
  );
}

ReminderToast.displayName = 'ReminderToast';
