import { useEffect, useState } from 'react';
import { Routes, Route, Navigate, Outlet, useNavigate } from 'react-router-dom';
import { listen } from '@tauri-apps/api/event';
import { sendNotification } from '@tauri-apps/plugin-notification';
import { useTranslation } from 'react-i18next';
import { AppShell } from './components/layout/AppShell';
import { Home } from './pages/Home';
import { Transfer } from './pages/Transfer';
import { Prompts } from './pages/Prompts';
import { CcHistory } from './pages/CcHistory';
import { Scratchpad } from './pages/Scratchpad';
import { ClaudeMd } from './pages/ClaudeMd';
import { Devices } from './pages/Devices';
import { Settings } from './pages/Settings';
import { Health } from './pages/Health';
import { Welcome } from './pages/Welcome';
import { DesignSystem } from './pages/DesignSystem';
import { Overlay } from './pages/Screenshot/Overlay';
import { configApi } from './api/config';
import { PERMISSION_ONBOARDED_KEY } from './hooks/usePermissions';

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
        const all = s.screenCapture.granted && s.inputMonitoring.granted;
        if (all) {
          localStorage.setItem(PERMISSION_ONBOARDED_KEY, '1');
          setState('pass');
        } else {
          // 启动主动引导：screenCapture 弹系统框（openSettings=false），
          // inputMonitoring 只能靠开设置面板引导（openSettings=true）。
          const reqs: Promise<unknown>[] = [];
          if (!s.screenCapture.granted) {
            reqs.push(configApi.requestPermission('screenCapture', false));
          }
          if (!s.inputMonitoring.granted) {
            reqs.push(configApi.requestPermission('inputMonitoring', true));
          }
          await Promise.all(reqs);
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
 * HealthReminderListener - 监听后端「久坐提醒」事件,弹系统通知。
 *
 * Business Logic: 后端 health daemon 判定连续工作达阈值且未贪睡、不在免打扰时段、
 *   notify_enabled 时 emit `health:reminder`;本组件监听后用 tauri-plugin-notification
 *   弹系统通知(标题/正文走 i18n)。挂在 <Routes> 同级,不影响路由渲染,仅副作用监听。
 */
function HealthReminderListener() {
  const { t } = useTranslation(['health', 'common']);
  useEffect(() => {
    const unlisten = listen('health:reminder', () => {
      void sendNotification({
        title: t('health:reminderTitle'),
        body: t('health:reminderBody'),
      });
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
        <Route path="/welcome" element={<Welcome />} />
        <Route element={<OnboardingGuard />}>
          <Route element={<AppShell />}>
            <Route path="/" element={<Home />} />
            <Route path="/transfer" element={<Transfer />} />
            <Route path="/prompts" element={<Prompts />} />
            <Route path="/cc-history" element={<CcHistory />} />
            <Route path="/scratchpad" element={<Scratchpad />} />
            <Route path="/claude-md" element={<ClaudeMd />} />
            <Route path="/devices" element={<Devices />} />
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
