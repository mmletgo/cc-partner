/**
 * HealthOverlay - 全屏健康提醒遮罩页（按模板渲染）。
 *
 * 独立于 AppShell/OnboardingGuard，路由 `/health-overlay?display={i}&template={id}`，
 * 旧 `type=water|reminder` 仍可解析。由 Tauri 透明置顶遮罩窗口直接加载。
 */
import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useSearchParams } from 'react-router-dom';
import { listen } from '@tauri-apps/api/event';
import { Button } from '@/components/primitives';
import { healthApi } from '@/api/health';
import type { HealthReminderTemplate } from '@/lib/types';
import { overlaySurfaceCopy, resolveOverlayTemplateId } from '@/lib/healthReminders';
import { computeRestLeft } from './healthOverlayCountdown';
import styles from './HealthOverlay.module.css';

type Mode = 'actions' | 'session';

interface HealthOverlaySurfaceProps {
  templateId: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   session 倒计时需要 MM:SS，避免各屏自己估秒。
 *
 * Code Logic（这个函数做什么）:
 *   把非负秒数格式化为两位分钟/秒。
 */
function formatMmSs(total: number): string {
  const s = Math.max(0, Math.floor(total));
  const m = Math.floor(s / 60);
  const sec = s % 60;
  return `${String(m).padStart(2, '0')}:${String(sec).padStart(2, '0')}`;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   每条模板共用同一套遮罩表面，不能再按饮水/休息硬分叉。
 *
 * Code Logic（这个组件做什么）:
 *   读 template query；用 key 在模板切换时整页复位，避免 effect 同步 setState。
 */
export default function HealthOverlay(): JSX.Element {
  const [searchParams] = useSearchParams();
  const templateId = useMemo(() => resolveOverlayTemplateId(searchParams), [searchParams]);
  return <HealthOverlaySurface key={templateId} templateId={templateId} />;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   单个模板的遮罩状态（配置、倒计时、动作）必须与当前 templateId 同生共死。
 *
 * Code Logic（这个组件做什么）:
 *   读配置；模板未返回前不画旧休息文案；instant 确认/跳过/推迟；session 开始后跟后端 endTs。
 */
function HealthOverlaySurface({ templateId }: HealthOverlaySurfaceProps): JSX.Element {
  const { t } = useTranslation(['health', 'common']);
  const [template, setTemplate] = useState<HealthReminderTemplate | null>(null);
  const [configLoaded, setConfigLoaded] = useState(false);
  const [mode, setMode] = useState<Mode>('actions');
  const [restLeft, setRestLeft] = useState(0);
  const [restEndTs, setRestEndTs] = useState<number | null>(null);

  useEffect(() => {
    document.documentElement.style.background = 'transparent';
    document.body.style.background = 'transparent';
  }, []);

  useEffect(() => {
    let cancelled = false;
    Promise.all([healthApi.getConfig(), healthApi.getStatus()])
      .then(([config, status]) => {
        if (cancelled) return;
        const found =
          config.reminders.find((item) => item.id === templateId) ??
          config.reminders.find((item) => item.id === status.overlayTemplateId) ??
          null;
        setTemplate(found);
        setConfigLoaded(true);
        const sameTemplate =
          !status.overlayTemplateId || status.overlayTemplateId === templateId;
        if (sameTemplate && typeof status.overlayRestEndTs === 'number') {
          setRestEndTs(status.overlayRestEndTs);
          setMode('session');
        }
      })
      .catch((e) => {
        console.error('读取健康遮罩配置失败', e);
        if (!cancelled) setConfigLoaded(true);
      });
    return () => {
      cancelled = true;
    };
  }, [templateId]);

  useEffect(() => {
    let active = true;
    const unlisten = listen<{ endTs: number; templateId?: string }>('health:rest-started', (e) => {
      if (!active) return;
      if (e.payload.templateId && e.payload.templateId !== templateId) return;
      setRestEndTs(e.payload.endTs);
      setMode('session');
    });
    return () => {
      active = false;
      void unlisten.then((fn) => fn());
    };
  }, [templateId]);

  useEffect(() => {
    if (mode !== 'session' || restEndTs == null) return;
    const tick = () => setRestLeft(computeRestLeft(restEndTs));
    tick();
    const id = setInterval(tick, 1000);
    return () => clearInterval(id);
  }, [mode, restEndTs]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   跳过/推迟只处理当前模板，再关当前遮罩（队列下一项由后端弹出）。
   *
   * Code Logic（这个函数做什么）:
   *   acknowledge skipped/snoozed；失败仍关窗。
   */
  const close = useCallback(
    async (snoozeMin?: number) => {
      try {
        if (snoozeMin) {
          await healthApi.acknowledge(templateId, 'snoozed', snoozeMin);
        } else {
          await healthApi.acknowledge(templateId, 'skipped');
        }
      } catch (e) {
        console.error('健康提醒操作失败', e);
      }
      await healthApi.closeOverlay();
    },
    [templateId],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   instant 点确认记一次；session 点主按钮开始倒计时。
   *
   * Code Logic（这个函数做什么）:
   *   instant → acknowledge completed；session → startSession。
   */
  const handleConfirm = useCallback(async () => {
    try {
      if (template?.complete === 'session') {
        const res = await healthApi.startSession(templateId);
        setRestEndTs(res.endTs);
        setMode('session');
        return;
      }
      await healthApi.acknowledge(templateId, 'completed');
    } catch (e) {
      console.error('确认健康提醒失败', e);
    }
    if (template?.complete !== 'session') {
      await healthApi.closeOverlay();
    }
  }, [template, templateId]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        healthApi.closeOverlay().catch((err) => console.error('关闭遮罩失败', err));
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  const copy = overlaySurfaceCopy(template, configLoaded);
  const title = copy?.title || (configLoaded ? t('health:reminderTitle') : '');
  const body = copy?.body || (configLoaded ? t('health:reminderBody') : '');
  const confirmLabel = copy?.confirmLabel || (configLoaded ? t('health:startRest') : '');
  const showActions = mode !== 'session' && (copy != null || configLoaded);

  return (
    <div className={styles.root}>
      <div className={styles.card}>
        {mode === 'session' ? (
          <>
            <h1 className={styles.title}>{t('health:resting')}</h1>
            <p className={styles.timer}>{formatMmSs(restLeft)}</p>
            <p className={styles.hint}>{t('health:escToClose')}</p>
          </>
        ) : showActions ? (
          <>
            <h1 className={styles.title}>{title}</h1>
            <p className={styles.body}>{body}</p>
            <div className={styles.actions}>
              <Button variant="primary" size="lg" onClick={() => void handleConfirm()}>
                {confirmLabel}
              </Button>
              <Button variant="secondary" size="lg" onClick={() => void close(5)}>
                {t('health:snooze5')}
              </Button>
              <Button variant="secondary" size="lg" onClick={() => void close(10)}>
                {t('health:snooze10')}
              </Button>
              <Button variant="ghost" size="lg" onClick={() => void close()}>
                {t('health:skip')}
              </Button>
            </div>
          </>
        ) : null}
      </div>
    </div>
  );
}
