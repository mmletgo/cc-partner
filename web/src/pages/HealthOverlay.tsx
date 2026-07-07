/**
 * HealthOverlay - 全屏健康提醒遮罩页（通用化，支持 reminder / water 两种类型）。
 *
 * 独立于 AppShell/OnboardingGuard，路由 `/health-overlay?display={i}&type=reminder|water`，
 * 由 Tauri 透明置顶遮罩窗口（每屏一个，label `health-overlay-{i}`）直接加载。窗口真透明，
 * 本页 onMount 强制 html/body `background:transparent` 覆盖全局主题底色（否则透明窗口会显示
 * 主题底色而非透出桌面=白屏）。页面自身用半透明黑色蒙层盖住整屏。
 *
 * type=reminder：中央展示久坐提醒文案 + 推迟 5/10 分钟 / 跳过 / 开始休息 按钮；点击「开始休息」
 *   进入 resting 态，从配置 breakSeconds 每秒倒数，到 0 自动 skip + 关闭遮罩。
 * type=water：中央展示喝水提醒文案 + 已饮水 / 跳过 / 延迟 5 / 延迟 10 分钟 按钮。
 * ESC 键（任意态）直接关闭遮罩，不调业务命令。
 *
 * 复用 `health` namespace 的 i18n 文案（reminderTitle/reminderBody/snooze5/snooze10/skip/
 * startRest/resting/escToClose/waterTitle/waterBody/drank/skipWater/snoozeWater5/snoozeWater10）。
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { useSearchParams } from 'react-router-dom';
import { healthApi } from '@/api/health';

type OverlayType = 'reminder' | 'water';
type Mode = 'actions' | 'resting';

/** 把秒数格式化为 MM:SS（breakSeconds 倒计时展示用）。 */
function formatMmSs(total: number): string {
  const s = Math.max(0, Math.floor(total));
  const m = Math.floor(s / 60);
  const sec = s % 60;
  return `${String(m).padStart(2, '0')}:${String(sec).padStart(2, '0')}`;
}

export default function HealthOverlay() {
  const { t } = useTranslation(['health', 'common']);
  const [searchParams] = useSearchParams();
  const type = (searchParams.get('type') as OverlayType | null) ?? 'reminder';

  const [breakSeconds, setBreakSeconds] = useState<number>(0);
  const [mode, setMode] = useState<Mode>('actions');
  const [restLeft, setRestLeft] = useState<number>(0);
  const restTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // onMount 强制透明：覆盖全局 reset.css 的 body 主题底色，使窗口透出真实桌面。
  useEffect(() => {
    document.documentElement.style.background = 'transparent';
    document.body.style.background = 'transparent';
  }, []);

  // onMount 取 breakSeconds（休息倒计时初始值，来自配置）。
  useEffect(() => {
    let cancelled = false;
    healthApi
      .getStatus()
      .then((st) => {
        if (!cancelled && typeof st.breakSeconds === 'number') {
          setBreakSeconds(st.breakSeconds);
        }
      })
      .catch((e) => console.error('读取健康状态失败', e));
    return () => {
      cancelled = true;
    };
  }, []);

  /** 关闭遮罩：业务命令失败也强关，避免困住用户。 */
  const close = useCallback(async (snoozeMin?: number) => {
    try {
      if (snoozeMin) {
        await healthApi.snooze(snoozeMin);
      } else {
        await healthApi.skip();
      }
    } catch (e) {
      console.error('健康提醒操作失败', e);
    }
    await healthApi.closeOverlay();
  }, []);

  /** 进入休息态：启动每秒倒数，到 0 自动 skip + 关闭遮罩。 */
  const startRest = useCallback(() => {
    if (restTimerRef.current) return;
    setMode('resting');
    setRestLeft(breakSeconds);
    restTimerRef.current = setInterval(() => {
      setRestLeft((n) => {
        if (n <= 1) {
          if (restTimerRef.current) {
            clearInterval(restTimerRef.current);
            restTimerRef.current = null;
          }
          (async () => {
            try {
              await healthApi.skip();
              await healthApi.recordRestCompleted();
            } catch (e) {
              console.error('休息结束记录失败', e);
            }
            await healthApi.closeOverlay();
          })();
          return 0;
        }
        return n - 1;
      });
    }, 1000);
  }, [breakSeconds]);

  // 卸载时清理倒计时 interval。
  useEffect(() => {
    return () => {
      if (restTimerRef.current) {
        clearInterval(restTimerRef.current);
        restTimerRef.current = null;
      }
    };
  }, []);

  // ESC 键：任意态直接关闭遮罩，不调业务命令。
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

  return (
    <div
      style={{
        width: '100vw',
        height: '100vh',
        background: 'rgba(0, 0, 0, 0.55)',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        color: '#fff',
        fontFamily: 'system-ui, -apple-system, sans-serif',
      }}
    >
      <div style={{ textAlign: 'center', maxWidth: 560, padding: '0 24px' }}>
        {type === 'water' ? (
          <WaterContent t={t} />
        ) : mode === 'resting' ? (
          <RestingContent restLeft={restLeft} t={t} />
        ) : (
          <ReminderActions close={close} startRest={startRest} t={t} />
        )}
      </div>
    </div>
  );
}

/** reminder 态：提醒文案 + 推迟 5/10 分钟 / 跳过 / 开始休息 按钮。 */
function ReminderActions({
  close,
  startRest,
  t,
}: {
  close: (snoozeMin?: number) => void;
  startRest: () => void;
  t: TFunction<'health'>;
}) {
  return (
    <>
      <h1 style={{ fontSize: 48, marginBottom: 16, fontWeight: 600 }}>
        {t('reminderTitle')}
      </h1>
      <p style={{ fontSize: 20, marginBottom: 32, lineHeight: 1.6, opacity: 0.92 }}>
        {t('reminderBody')}
      </p>
      <div style={{ display: 'flex', gap: 16, justifyContent: 'center', flexWrap: 'wrap' }}>
        <button onClick={() => close(5)} style={btnStyle}>
          {t('snooze5')}
        </button>
        <button onClick={() => close(10)} style={btnStyle}>
          {t('snooze10')}
        </button>
        <button onClick={() => close()} style={{ ...btnStyle, background: 'rgba(255,255,255,0.12)' }}>
          {t('skip')}
        </button>
        <button onClick={startRest} style={{ ...btnStyle, background: 'rgba(80,200,160,0.35)' }}>
          {t('startRest')}
        </button>
      </div>
    </>
  );
}

/** reminder resting 态：休息中 + 倒计时 MM:SS + ESC 提示（无操作按钮）。 */
function RestingContent({
  restLeft,
  t,
}: {
  restLeft: number;
  t: TFunction<'health'>;
}) {
  return (
    <>
      <h1 style={{ fontSize: 44, marginBottom: 16, fontWeight: 600 }}>
        {t('resting')}
      </h1>
      <p
        style={{
          fontSize: 64,
          marginBottom: 24,
          fontWeight: 700,
          fontVariantNumeric: 'tabular-nums',
        }}
      >
        {formatMmSs(restLeft)}
      </p>
      <p style={{ fontSize: 16, opacity: 0.75 }}>{t('escToClose')}</p>
    </>
  );
}

/** water 态：喝水文案 + 已饮水 / 跳过 / 延迟 5 / 延迟 10 分钟 按钮。 */
function WaterContent({ t }: { t: TFunction<'health'> }) {
  const onDrank = async () => {
    try {
      await healthApi.recordWater();
    } catch (e) {
      console.error('记录喝水失败', e);
    }
    await healthApi.closeOverlay();
  };
  const onSkip = async () => {
    try {
      await healthApi.skipWater();
    } catch (e) {
      console.error('跳过喝水提醒失败', e);
    }
    await healthApi.closeOverlay();
  };
  const onSnooze = async (minutes: number) => {
    try {
      await healthApi.snoozeWater(minutes);
    } catch (e) {
      console.error('延迟喝水提醒失败', e);
    }
    await healthApi.closeOverlay();
  };
  return (
    <>
      <h1 style={{ fontSize: 48, marginBottom: 16, fontWeight: 600 }}>
        {t('waterTitle')}
      </h1>
      <p style={{ fontSize: 20, marginBottom: 32, lineHeight: 1.6, opacity: 0.92 }}>
        {t('waterBody')}
      </p>
      <div style={{ display: 'flex', gap: 16, justifyContent: 'center', flexWrap: 'wrap' }}>
        <button onClick={onDrank} style={{ ...btnStyle, background: 'rgba(80,160,255,0.35)' }}>
          {t('drank')}
        </button>
        <button onClick={() => onSnooze(5)} style={btnStyle}>
          {t('snoozeWater5')}
        </button>
        <button onClick={() => onSnooze(10)} style={btnStyle}>
          {t('snoozeWater10')}
        </button>
        <button onClick={onSkip} style={{ ...btnStyle, background: 'rgba(255,255,255,0.12)' }}>
          {t('skipWater')}
        </button>
      </div>
    </>
  );
}

const btnStyle: React.CSSProperties = {
  fontSize: 16,
  padding: '12px 24px',
  borderRadius: 8,
  border: '1px solid rgba(255,255,255,0.3)',
  background: 'rgba(255,255,255,0.2)',
  color: '#fff',
  cursor: 'pointer',
  minWidth: 140,
};
