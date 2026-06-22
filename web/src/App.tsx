import { useEffect, useState } from 'react';
import { Routes, Route, Navigate, Outlet } from 'react-router-dom';
import { AppShell } from './components/layout/AppShell';
import { Home } from './pages/Home';
import { Transfer } from './pages/Transfer';
import { Prompts } from './pages/Prompts';
import { Scratchpad } from './pages/Scratchpad';
import { Devices } from './pages/Devices';
import { Settings } from './pages/Settings';
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
          setState('redirect');
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

export default function App() {
  return (
    <Routes>
      {/* 区域截图选区页：独立于 AppShell/OnboardingGuard，由 Tauri 选区窗口直接加载 */}
      <Route path="/screenshot-overlay" element={<Overlay />} />
      <Route path="/welcome" element={<Welcome />} />
      <Route element={<OnboardingGuard />}>
        <Route element={<AppShell />}>
          <Route path="/" element={<Home />} />
          <Route path="/transfer" element={<Transfer />} />
          <Route path="/prompts" element={<Prompts />} />
          <Route path="/scratchpad" element={<Scratchpad />} />
          <Route path="/devices" element={<Devices />} />
          <Route path="/settings" element={<Settings />} />
          {isDev && <Route path="/design-system" element={<DesignSystem />} />}
          <Route path="*" element={<Navigate to="/" replace />} />
        </Route>
      </Route>
    </Routes>
  );
}
