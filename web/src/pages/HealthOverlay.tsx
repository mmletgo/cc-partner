/**
 * HealthOverlay - 全屏健康提醒遮罩页。
 *
 * 独立于 AppShell/OnboardingGuard,路由 `/health-overlay?display={i}`,由 Tauri 透明置顶
 * 遮罩窗口(每屏一个,label `health-overlay-{i}`)直接加载。窗口真透明,本页 onMount 强制
 * html/body `background:transparent` 覆盖全局主题底色(否则透明窗口会显示主题底色而非透出桌面=白屏)。
 * 页面自身用半透明黑色蒙层盖住整屏,中央展示提醒文案 + 推迟 5/10 分钟 / 跳过按钮,
 * 点击后调 `snooze_reminder`/`skip_reminder` + `close_health_overlay` 关闭全部遮罩窗口。
 *
 * 复用 `health` namespace 的 i18n 文案(reminderTitle/reminderBody/snooze5/snooze10/skip)。
 */
import { useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';

export default function HealthOverlay() {
  const { t } = useTranslation(['health', 'common']);

  // onMount 强制透明:覆盖全局 reset.css 的 body 主题底色,使窗口透出真实桌面。
  useEffect(() => {
    document.documentElement.style.background = 'transparent';
    document.body.style.background = 'transparent';
  }, []);

  /**
   * 关闭遮罩:有 snoozeMin 则推迟 N 分钟,否则跳过本次;最后关闭所有遮罩窗口。
   */
  const close = async (snoozeMin?: number) => {
    try {
      if (snoozeMin) {
        await invoke('snooze_reminder', { minutes: snoozeMin });
      } else {
        await invoke('skip_reminder');
      }
    } catch (e) {
      // 即使推迟/跳过失败也要关闭遮罩,避免困住用户。
      console.error('健康提醒操作失败', e);
    }
    await invoke('close_health_overlay');
  };

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
        <h1 style={{ fontSize: 48, marginBottom: 16, fontWeight: 600 }}>
          {t('health:reminderTitle')}
        </h1>
        <p style={{ fontSize: 20, marginBottom: 32, lineHeight: 1.6, opacity: 0.92 }}>
          {t('health:reminderBody')}
        </p>
        <div style={{ display: 'flex', gap: 16, justifyContent: 'center', flexWrap: 'wrap' }}>
          <button
            onClick={() => close(5)}
            style={btnStyle}
          >
            {t('health:snooze5')}
          </button>
          <button
            onClick={() => close(10)}
            style={btnStyle}
          >
            {t('health:snooze10')}
          </button>
          <button
            onClick={() => close()}
            style={{ ...btnStyle, background: 'rgba(255,255,255,0.12)' }}
          >
            {t('health:skip')}
          </button>
        </div>
      </div>
    </div>
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
