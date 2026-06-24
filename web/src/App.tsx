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

interface TauriInternalsWindow extends Window {
  __TAURI_INTERNALS__?: {
    transformCallback?: unknown;
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   生产桌面端运行在 Tauri 内，顶层事件监听依赖 Tauri event internals；但 Playwright/Vite 浏览器调试
 *   会在普通浏览器中加载同一路由，缺少 internals 时不应让页面白屏或抛未处理异常。
 *
 * Code Logic（这个函数做什么）:
 *   检测 window.__TAURI_INTERNALS__.transformCallback 是否存在且为函数，作为是否可注册 Tauri event listener 的边界。
 */
function canListenToTauriEvents(): boolean {
  const internals = (window as TauriInternalsWindow).__TAURI_INTERNALS__;
  return typeof internals?.transformCallback === 'function';
}

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
    if (!canListenToTauriEvents()) return undefined;
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
 * HealthReminderListener - 健康提醒系统通知监听(久坐 + 喝水)
 *
 * Business Logic（为什么需要这个组件）:
 *   后端 health daemon 在久坐达阈值(且未贪睡/不在免打扰/notify_enabled)时 emit
 *   `health:reminder`,在喝水提醒时点 emit `health:water`。app 最小化/在后台时应用内
 *   toast 看不见,需要原生系统通知触达用户。应用内 toast 已停用,系统通知成为久坐/
 *   喝水的主提醒方式(久坐另有全屏遮罩 HealthOverlay 互补)。挂在 App 顶层(与
 *   PermissionNeededListener 同层),任意路由下都生效。
 *
 * Code Logic（这个组件做什么）:
 *   - listen `health:reminder` → 系统通知(reminderTitle/reminderBody)
 *   - listen `health:water` → 系统通知(waterTitle/waterBody)
 *   - notify helper:checkNotificationGranted(复用 lib/notification)授权才发,失败静默
 *   - 标题/正文走 i18n health ns,随当前语言切换;hooks 在 early return 之前(项目规则 20)
 */
function HealthReminderListener() {
  const { t } = useTranslation(['health']);
  useEffect(() => {
    if (!canListenToTauriEvents()) return undefined;
    // 发系统通知:授权才发,失败静默(系统通知是久坐/喝水的主提醒通道)
    const notify = async (title: string, body: string) => {
      try {
        if (!(await checkNotificationGranted())) return;
        sendNotification({ title, body });
      } catch {
        // 未授权通知权限或发送失败时静默
      }
    };
    const reminderUnlisten = listen('health:reminder', () =>
      void notify(t('health:reminderTitle'), t('health:reminderBody')),
    );
    const waterUnlisten = listen('health:water', () =>
      void notify(t('health:waterTitle'), t('health:waterBody')),
    );
    return () => {
      void reminderUnlisten.then((fn) => fn());
      void waterUnlisten.then((fn) => fn());
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
