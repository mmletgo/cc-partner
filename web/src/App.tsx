import { useEffect, useState } from 'react';
import { Routes, Route, Navigate, Outlet, useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { listen } from '@tauri-apps/api/event';
import { sendNotification } from '@tauri-apps/plugin-notification';
import { AppShell } from './components/layout/AppShell';
import { Home } from './pages/Home';
import { Transfer } from './pages/Transfer';
import { Prompts } from './pages/Prompts';
import { CcHistory } from './pages/CcHistory';
import { Scratchpad } from './pages/Scratchpad';
import { PromptOptimizer } from './pages/PromptOptimizer';
import { ClaudeMd } from './pages/ClaudeMd';
import { ClaudeCodeAssets } from './pages/ClaudeCodeAssets';
import { Devices } from './pages/Devices';
import { Settings } from './pages/Settings';
import { Health } from './pages/Health';
import { Welcome } from './pages/Welcome';
import { DesignSystem } from './pages/DesignSystem';
import { Overlay } from './pages/Screenshot/Overlay';
import HealthOverlay from './pages/HealthOverlay';
import { configApi } from './api/config';
import { PERMISSION_ONBOARDED_KEY } from './hooks/usePermissions';
import { checkNotificationGranted } from './lib/notification';

const isDev = import.meta.env.DEV;

type GuardState = 'loading' | 'pass' | 'redirect';

/**
 * OnboardingGuard - 首次启动权限引导守卫
 *
 * Business Logic（为什么需要这个组件）:
 *   仅在「首次启动且权限未全部就绪」时把用户导向 /welcome 一次。
 *   已完成引导（localStorage 标记）或权限已就绪则直接放行，避免每次启动重复打扰。
 *
 * Code Logic（这个组件做什么）:
 *   - 读 PERMISSION_ONBOARDED_KEY，已标记 → pass
 *   - 否则查权限：全部授权 → 写标记 + pass；否则 → redirect 到 /welcome
 *   - 查询失败 → pass（不阻塞用户）
 *   - hooks 在 early return 之前（React 规则：hooks 调用顺序不能条件化）
 */
function OnboardingGuard() {
  const [state, setState] = useState<GuardState>('loading');

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      if (localStorage.getItem(PERMISSION_ONBOARDED_KEY) === '1') {
        if (!cancelled) setState('pass');
        return;
      }
      try {
        const s = await configApi.permissions();
        if (cancelled) return;
        const all =
          s.screenCapture.granted &&
          s.accessibility.granted &&
          s.inputMonitoring.granted;
        if (all) {
          localStorage.setItem(PERMISSION_ONBOARDED_KEY, '1');
          setState('pass');
        } else {
          // 首启不主动 request/openSettings（避免自动弹出系统设置面板打扰用户），
          // 仅跳转 Welcome 页，由用户主动点「去设置」逐项引导授权。
          if (!cancelled) setState('redirect');
        }
      } catch {
        if (!cancelled) setState('pass');
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  if (state === 'loading') return null;
  if (state === 'redirect') return <Navigate to="/welcome" replace />;
  return <Outlet />;
}

/**
 * PermissionNeededListener - 监听后端「截图需要屏幕录制权限」事件,导航到引导页。
 *
 * Business Logic: 用户按截图快捷键 / 托盘截图但屏幕录制未授权时,后端已显示主窗口并 emit
 *   `screenshot:permission-needed`;本组件监听后跳 /welcome 引导授权,避免抓到空白图。
 *   挂在 <Routes> 同级(BrowserRouter 内),不影响路由渲染,仅副作用监听。
 */
function PermissionNeededListener() {
  const navigate = useNavigate();
  useEffect(() => {
    const unlisten = listen('screenshot:permission-needed', () => {
      navigate('/welcome', { replace: true });
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [navigate]);
  return null;
}

/**
 * HealthReminderListener - 健康提醒系统通知监听
 *
 * Business Logic（为什么需要这个组件）:
 *   后端 health daemon 判定连续工作达阈值(且未贪睡/不在免打扰/notify_enabled)时 emit
 *   `health:reminder`。app 最小化/在后台时应用内 toast(ReminderToast)看不见,需要原生
 *   系统通知触达用户。本组件监听后弹系统通知,与应用内 toast/全屏遮罩互补。挂在 App
 *   顶层(与 PermissionNeededListener 同层),任意路由下都生效。
 *
 * Code Logic（这个组件做什么）:
 *   - listen `health:reminder` → checkNotificationGranted(复用 lib/notification)→ sendNotification
 *   - 未授权或发送失败静默(应用内 toast/全屏遮罩仍会触发)
 *   - 标题/正文走 i18n health ns(reminderTitle/reminderBody),随当前语言切换
 *   - hooks 在 early return 之前(项目规则 20)
 */
function HealthReminderListener() {
  const { t } = useTranslation(['health']);
  useEffect(() => {
    const unlisten = listen('health:reminder', async () => {
      try {
        if (!(await checkNotificationGranted())) return;
        sendNotification({
          title: t('health:reminderTitle'),
          body: t('health:reminderBody'),
        });
      } catch {
        // 未授权通知权限或发送失败时静默;应用内 toast/全屏遮罩仍会触发
      }
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [t]);
  return null;
}

export default function App() {
  return (
    <>
      <PermissionNeededListener />
      <HealthReminderListener />
      <Routes>
        {/* 区域截图选区页：独立于 AppShell/OnboardingGuard，由 Tauri 选区窗口直接加载 */}
        <Route path="/screenshot-overlay" element={<Overlay />} />
        {/* 全屏健康提醒遮罩页：独立于 AppShell/OnboardingGuard，由 Tauri 透明置顶遮罩窗口直接加载 */}
        <Route path="/health-overlay" element={<HealthOverlay />} />
        <Route path="/welcome" element={<Welcome />} />
        <Route element={<OnboardingGuard />}>
          <Route element={<AppShell />}>
            <Route path="/" element={<Home />} />
            <Route path="/transfer" element={<Transfer />} />
            <Route path="/prompts" element={<Prompts />} />
            <Route path="/cc-history" element={<CcHistory />} />
            <Route path="/scratchpad" element={<Scratchpad />} />
            <Route path="/prompt-optimizer" element={<PromptOptimizer />} />
            <Route path="/claude-md" element={<ClaudeMd />} />
            <Route path="/claude-code" element={<ClaudeCodeAssets />} />
            <Route path="/devices" element={<Devices />} />
            <Route path="/ssh" element={<Navigate to="/devices" replace />} />
            <Route path="/settings" element={<Settings />} />
            <Route path="/health" element={<Health />} />
            {isDev && <Route path="/design-system" element={<DesignSystem />} />}
            <Route path="*" element={<Navigate to="/" replace />} />
          </Route>
        </Route>
      </Routes>
    </>
  );
}
