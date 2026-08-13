import { lazy, Suspense, useEffect, useState, type ComponentType, type ReactNode } from 'react';
import { Routes, Route, Navigate, Outlet, useLocation, useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { sendNotification } from '@tauri-apps/plugin-notification';
import { Button, Card, Dialog } from '@/components/primitives';
import { AppShell } from './components/layout/AppShell';
import { RouteErrorBoundary } from './components/layout/RouteErrorBoundary';
import { configApi } from './api/config';
import {
  permissionOnboardedKey,
  permissionSkippedKey,
  type AppFlavor,
} from './hooks/usePermissions';
import { WorkbenchProjectsProvider } from './hooks/useWorkbenchProjects';
import { WorkbenchAgentHintsProvider } from './hooks/WorkbenchAgentHintsProvider';
import { WorkbenchDependencyProvider } from './hooks/useWorkbenchDependency';
import { WorkbenchTerminalBuffersProvider } from './hooks/useWorkbenchTerminalBuffers';
import { AttentionProvider } from './hooks/useAttention';
import { ScratchpadAutosaveProvider } from './hooks/ScratchpadAutosaveProvider';
import { OperationalNotificationCoordinator } from './hooks/useOperationalNotifications';
import { useWorkbenchWindowRole } from './hooks/useWorkbenchWindowRole';
import { attentionApi } from './api/attention';
import { checkNotificationGranted } from './lib/notification';
import { backendApi } from './api/backend';
import { flushPendingWritesThenClose } from './lib/closeFlush';
import { pendingWrites } from './lib/pendingWrites';
import { shouldMountGlobalWindowListeners } from './lib/workbenchWindow';
import { buildWorkbenchDeepLink } from './pages/Workbench/workbenchDeepLink';
import { LanDisclosureGate } from './LanDisclosureGate';
import styles from './App.module.css';

const isDev = import.meta.env.DEV;

/**
 * Business Logic（为什么需要这个函数）:
 *   页面 barrel 多为 named export，而 React.lazy 要求 default；需统一适配避免改每个页面。
 *
 * Code Logic（这个函数做什么）:
 *   动态 import 模块后把 `module[name]` 包装为 `{ default: module.Name }` 供 lazy 使用。
 */
function lazyNamed<TModule extends Record<string, unknown>, TName extends keyof TModule>(
  loader: () => Promise<TModule>,
  name: TName,
) {
  return lazy(async () => {
    const module = await loader();
    const Component = module[name] as ComponentType;
    return { default: Component };
  });
}

// AppShell 内业务路由：全部 lazy，initial graph 不携带页面重型依赖
const Home = lazyNamed(() => import('./pages/Home'), 'Home');
const Attention = lazyNamed(() => import('./pages/Attention'), 'Attention');
const Transfer = lazyNamed(() => import('./pages/Transfer'), 'Transfer');
const Prompts = lazyNamed(() => import('./pages/Prompts'), 'Prompts');
const CcHistory = lazyNamed(() => import('./pages/CcHistory'), 'CcHistory');
const Workbench = lazyNamed(() => import('./pages/Workbench'), 'Workbench');
const Scratchpad = lazyNamed(() => import('./pages/Scratchpad'), 'Scratchpad');
const PromptOptimizer = lazyNamed(() => import('./pages/PromptOptimizer'), 'PromptOptimizer');
// ClaudeMd page module retained under pages/ for N/N+1; ClaudeCodeAssets frontend deleted after portable parity E2E.
// ownership routes redirect to Agent Hub. Gate D Task 7: old routes stay registered
// but hidden; only /agent-hub is the new UI entry until N+2 removal evidence lands.
const AgentHub = lazyNamed(() => import('./pages/AgentHub'), 'AgentHub');
const ProviderManager = lazyNamed(() => import('./pages/ProviderManager'), 'ProviderManager');
const Settings = lazyNamed(() => import('./pages/Settings'), 'Settings');
const Health = lazyNamed(() => import('./pages/Health'), 'Health');
const ActivityStats = lazyNamed(() => import('./pages/ActivityStats'), 'ActivityStats');
const Welcome = lazyNamed(() => import('./pages/Welcome'), 'Welcome');
const Overlay = lazyNamed(() => import('./pages/Screenshot/Overlay'), 'Overlay');
const HealthOverlay = lazy(() => import('./pages/HealthOverlay'));

/**
 * Business Logic（为什么需要这个函数）:
 *   DesignSystem 仅开发预览，不得进入生产静态/同步依赖图。
 *
 * Code Logic（这个函数做什么）:
 *   仅在 isDev 时创建 lazy 组件；生产返回 null。
 */
function createDesignSystemLazy(): React.LazyExoticComponent<ComponentType> | null {
  if (!isDev) return null;
  return lazy(() =>
    import('./pages/DesignSystem').then((module) => ({ default: module.DesignSystem })),
  );
}

const DesignSystem = createDesignSystemLazy();

/**
 * Business Logic（为什么需要这个组件）:
 *   lazy route 加载 chunk 期间需要稳定占位，避免 main 区域空白闪烁。
 *
 * Code Logic（这个组件做什么）:
 *   Suspense fallback 展示 common:loading。
 */
function RouteLoadingFallback(): ReactNode {
  const { t } = useTranslation(['common']);
  return (
    <div className={styles.routeLoading} data-testid="route-loading">
      {t('common:loading')}
    </div>
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   AppShell 内每个路由需独立 Suspense + error boundary，pathname 变化自动复位错误。
 *
 * Code Logic（这个组件做什么）:
 *   读取 location.pathname 作为 resetKey，包裹 Suspense 与 RouteErrorBoundary。
 */
function ShellRoute({ children }: { children: ReactNode }): ReactNode {
  const { pathname } = useLocation();
  return (
    <RouteErrorBoundary resetKey={pathname}>
      <Suspense fallback={<RouteLoadingFallback />}>{children}</Suspense>
    </RouteErrorBoundary>
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   截图/健康 overlay 与欢迎页不在 AppShell 内，但仍需错误隔离，避免白屏困住用户。
 *
 * Code Logic（这个组件做什么）:
 *   使用固定 routeKey 作为 resetKey，包裹 Suspense + RouteErrorBoundary。
 */
function IsolatedRoute({
  routeKey,
  children,
}: {
  routeKey: string;
  children: ReactNode;
}): ReactNode {
  return (
    <RouteErrorBoundary resetKey={routeKey}>
      <Suspense fallback={<RouteLoadingFallback />}>{children}</Suspense>
    </RouteErrorBoundary>
  );
}

/**
 * DEV/E2E 路由崩溃夹具。
 *
 * Business Logic（为什么需要这个组件）:
 *   frontend-foundation E2E 需要可复现的「路由 render throw → boundary 兜底 → 重试恢复」路径，
 *   且不得把崩溃夹具带进生产 bundle。
 *
 * Code Logic（这个组件做什么）:
 *   仅 DEV 注册；当 sessionStorage `cp-force-route-error=1` 时 throw，否则渲染可测 ok 标记。
 */
function DevRouteErrorFixture(): ReactNode {
  if (
    typeof sessionStorage !== 'undefined' &&
    sessionStorage.getItem('cp-force-route-error') === '1'
  ) {
    throw new Error('cp-force-route-error');
  }
  // 测试只依赖 data-testid；给最小可见盒避免 Playwright 把空节点判 hidden；无 letterful 文案
  return (
    <div
      data-testid="route-error-fixture-ok"
      style={{ width: 1, height: 1, overflow: 'hidden' }}
      aria-hidden="true"
    />
  );
}

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
 *   三项产品权限未齐时导向 /welcome；开发壳与发布版引导标记隔离。
 *   「暂时跳过」与「已全部授权」分 key，缺权限时不得因旧 onboarded 标记绕过 Welcome。
 *
 * Code Logic（这个组件做什么）:
 *   - resolve flavor（get_app_identity；失败当 release）→ flavor 专属 onboarded/skipped key
 *   - 一次 check_permissions：屏幕录制、辅助功能、通知全部 granted → 写 onboarded、清 skipped → pass
 *   - 未齐但 skipped=1 → pass（用户明确跳过）
 *   - 否则 → redirect /welcome（含仅有旧 onboarded、未真正授权的情况）
 *   - 权限查询失败 → pass（不永久卡死）
 *   - hooks 在 early return 之前
 */
function OnboardingGuard() {
  const [state, setState] = useState<GuardState>('loading');

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      let flavor: AppFlavor = 'release';
      try {
        const identity = await configApi.appIdentity();
        if (identity.flavor === 'dev' || identity.flavor === 'release') {
          flavor = identity.flavor;
        }
      } catch {
        // 浏览器调试 / 旧后端：按 release key
      }
      if (cancelled) return;

      const onboardedKey = permissionOnboardedKey(flavor);
      const skippedKey = permissionSkippedKey(flavor);

      try {
        // 展示权限一律以 Rust check_permissions 为权威（含 notification）；
        // 禁止再并行 checkNotificationGranted 二次拉取，避免双路径判定漂移。
        const s = await configApi.permissions();
        if (cancelled) return;
        const all =
          s.screenCapture.granted &&
          s.accessibility.granted &&
          s.notification.granted;
        if (all) {
          localStorage.setItem(onboardedKey, '1');
          localStorage.removeItem(skippedKey);
          setState('pass');
          return;
        }
        if (localStorage.getItem(skippedKey) === '1') {
          setState('pass');
          return;
        }
        // 首启 / 未授权：不自动 openSettings，只进 Welcome
        setState('redirect');
      } catch {
        // 查询失败：若用户曾明确跳过则放行，否则也放行以免卡死（与历史行为一致）
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
  const { label } = useWorkbenchWindowRole();
  useEffect(() => {
    if (!shouldMountGlobalWindowListeners(label)) return undefined;
    if (!canListenToTauriEvents()) return undefined;
    const unlisten = listen('screenshot:permission-needed', () => {
      navigate('/welcome', { replace: true });
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [label, navigate]);
  return null;
}

/**
 * HealthReminderListener - 健康提醒系统通知监听
 *
 * Business Logic（为什么需要这个组件）:
 *   后端按模板 emit `health:reminder`（载荷含 title/body）。app 最小化时应用内
 *   toast 看不见,需要原生系统通知。应用内 toast 已停用；遮罩走 HealthOverlay。
 *
 * Code Logic（这个组件做什么）:
 *   listen `health:reminder` → 用载荷 title/body 发系统通知；缺字段回落 i18n。
 */
function HealthReminderListener() {
  const { t } = useTranslation(['health']);
  const { label } = useWorkbenchWindowRole();
  useEffect(() => {
    if (!shouldMountGlobalWindowListeners(label)) return undefined;
    if (!canListenToTauriEvents()) return undefined;
    const notify = async (title: string, body: string) => {
      try {
        if (!(await checkNotificationGranted())) return;
        sendNotification({ title, body });
      } catch {
        // 未授权通知权限或发送失败时静默
      }
    };
    const reminderUnlisten = listen<{ templateId?: string; title?: string; body?: string }>(
      'health:reminder',
      (event) =>
        void notify(
          event.payload.title || t('health:reminderTitle'),
          event.payload.body || t('health:reminderBody'),
        ),
    );
    return () => {
      void reminderUnlisten.then((fn) => fn());
    };
  }, [label, t]);
  return null;
}

/**
 * WorkbenchDeepLinkListener - 接收他窗投递的工作台深链。
 *
 * Business Logic（为什么需要这个组件）:
 *   Inbox / 执行现场可能要落到已占用该项目的卫星窗；该窗必须应用 query 而不能改主窗项目。
 *
 * Code Logic（这个组件做什么）:
 *   listen `workbench:apply-deeplink` 后 navigate 到 build 出的 `/workbench?...`。
 */
function WorkbenchDeepLinkListener() {
  const navigate = useNavigate();
  useEffect(() => {
    if (!canListenToTauriEvents()) return undefined;
    const unlisten = listen('workbench:apply-deeplink', (event) => {
      const raw = event.payload;
      if (!raw || typeof raw !== 'object') return;
      const record = raw as Record<string, unknown>;
      const read = (key: string): string | null => {
        const value = record[key];
        return typeof value === 'string' && value.trim() ? value.trim() : null;
      };
      const viewRaw = read('view');
      navigate(
        buildWorkbenchDeepLink({
          projectId: read('projectId'),
          worktreeId: read('worktreeId'),
          sessionId: read('sessionId'),
          view: viewRaw === 'automation' || viewRaw === 'files' ? viewRaw : null,
          taskId: read('taskId'),
          outboxId: read('outboxId'),
          path: read('path'),
        }),
      );
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [navigate]);
  return null;
}

/**
 * BackendCloseChoiceListener props（测试可注入初始 open）。
 *
 * Business Logic（为什么需要这个类型）:
 *   单测无法稳定触发 Tauri onCloseRequested，需要可直接打开对话框验证 flush 门闩。
 *
 * Code Logic（字段说明）:
 *   initialOpenForTest 仅用于测试；生产默认 false。
 */
export interface BackendCloseChoiceListenerProps {
  initialOpenForTest?: boolean;
}

/**
 * BackendCloseChoiceListener - GUI 关闭时选择是否同时停止后台 sidecar。
 *
 * Business Logic（为什么需要这个组件）:
 *   GUI 关闭不能再直接退出进程；用户可能希望仅关闭桌面窗口并保留后台后端继续为手机/局域网服务，
 *   也可能希望完整关闭前后端。托盘退出与窗口关闭必须进入同一选择流程。
 *   关闭前必须 await 全部 pending write（如速记本），失败时中止关闭并在对话框展示错误。
 *
 * Code Logic（这个组件做什么）:
 *   - 仅在 Tauri 主窗口 label=`main` 时注册关闭监听，避免截图/健康 overlay 辅助窗口被拦截
 *   - 主窗口监听 Tauri `getCurrentWindow().onCloseRequested` 并 `preventDefault()`
 *   - 主窗口监听 Rust 托盘 emit 的 `backend:close-requested`
 *   - 共享 Dialog 原语承载选择 UI（portal / focus trap / Escape / backdrop）
 *   - busy（closingMode !== null）时 closeOnEscape/closeOnBackdrop=false，onClose early return
 *   - modal 中两条关闭路径都先 `await pendingWrites.flushAll()`，再 stop/exit
 *   - flush 失败保持对话框打开、复位 busy、展示 close-dialog error
 *   - hooks 全部在条件渲染之前；open=false 时由 Dialog 返回 null
 */
export function BackendCloseChoiceListener({
  initialOpenForTest = false,
}: BackendCloseChoiceListenerProps = {}) {
  const { t } = useTranslation(['common']);
  const [open, setOpen] = useState(initialOpenForTest);
  const [closingMode, setClosingMode] = useState<'gui' | 'full' | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!canListenToTauriEvents()) return undefined;
    const currentWindow = getCurrentWindow();
    if (currentWindow.label !== 'main') return undefined;

    const closeUnlisten = currentWindow.onCloseRequested((event) => {
      event.preventDefault();
      setError(null);
      setOpen(true);
    });
    const trayUnlisten = listen('backend:close-requested', () => {
      setError(null);
      setOpen(true);
    });
    return () => {
      void closeUnlisten.then((fn) => fn());
      void trayUnlisten.then((fn) => fn());
    };
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   仅关闭 GUI 时仍需先落库 pending write，避免静默丢正文。
   *
   * Code Logic（这个函数做什么）:
   *   设置 busy → await flushPendingWritesThenClose(gui) → 失败复位 busy 并展示错误。
   */
  const handleGuiOnlyClose = async () => {
    setClosingMode('gui');
    setError(null);
    try {
      await flushPendingWritesThenClose('gui', {
        flushAll: () => pendingWrites.flushAll(),
        stop: () => backendApi.stop(),
        exitGui: () => backendApi.exitGui(),
      });
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      setClosingMode(null);
    }
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   前后端都关闭前必须先 flush pending write，再停 sidecar 与退出 GUI。
   *
   * Code Logic（这个函数做什么）:
   *   设置 busy → await flushPendingWritesThenClose(full) → 失败保持对话框。
   */
  const handleFullClose = async () => {
    setClosingMode('full');
    setError(null);
    try {
      await flushPendingWritesThenClose('full', {
        flushAll: () => pendingWrites.flushAll(),
        stop: () => backendApi.stop(),
        exitGui: () => backendApi.exitGui(),
      });
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      setClosingMode(null);
    }
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户取消关闭选择时必须能回到应用；但 flush/stop 进行中禁止丢弃 busy 状态。
   *
   * Code Logic（这个函数做什么）:
   *   closingMode 非 null 时 early return；否则关闭对话框并清空错误。
   */
  const handleCancelClose = () => {
    if (closingMode !== null) return;
    setOpen(false);
    setError(null);
  };

  return (
    <Dialog
      open={open}
      titleId="backend-close-title"
      onClose={handleCancelClose}
      className={styles.closeDialog}
      closeOnEscape={closingMode === null}
      closeOnBackdrop={closingMode === null}
    >
      <Card variant="elevated" className={styles.closeDialogCard}>
        <Card.Header>
          <h2 id="backend-close-title" className={styles.closeDialogTitle}>
            {t('common:backendClose.title')}
          </h2>
        </Card.Header>
        <Card.Body>
          <p className={styles.closeDialogText}>{t('common:backendClose.description')}</p>
          {error ? (
            <p className={styles.closeDialogError}>
              {t('common:backendClose.error', { error })}
            </p>
          ) : null}
        </Card.Body>
        <Card.Footer>
          <Button variant="ghost" onClick={handleCancelClose} disabled={closingMode !== null}>
            {t('common:backendClose.cancel')}
          </Button>
          <Button
            variant="secondary"
            onClick={handleGuiOnlyClose}
            loading={closingMode === 'gui'}
            disabled={closingMode === 'full'}
          >
            {t('common:backendClose.guiOnly')}
          </Button>
          <Button
            variant="danger"
            onClick={handleFullClose}
            loading={closingMode === 'full'}
            disabled={closingMode === 'gui'}
          >
            {t('common:backendClose.stopBackend')}
          </Button>
        </Card.Footer>
      </Card>
    </Dialog>
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   卫星窗复用同一 React 树，但权限引导、健康系统通知只能由主窗处理。
 *
 * Code Logic（这个组件做什么）:
 *   仅 main 挂载 PermissionNeededListener 与 HealthReminderListener。
 */
function MainWindowOnlyListeners() {
  const { label } = useWorkbenchWindowRole();
  if (!shouldMountGlobalWindowListeners(label)) return null;
  return (
    <>
      <PermissionNeededListener />
      <HealthReminderListener />
    </>
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   运营通知协调器每窗一份会重复发 OS 通知。
 *
 * Code Logic（这个组件做什么）:
 *   仅 main 渲染 OperationalNotificationCoordinator。
 */
function MainWindowOperationalNotifications() {
  const { label } = useWorkbenchWindowRole();
  if (!shouldMountGlobalWindowListeners(label)) return null;
  return <OperationalNotificationCoordinator />;
}

export default function App() {
  return (
    <LanDisclosureGate>
      <MainWindowOnlyListeners />
      <WorkbenchDeepLinkListener />
      <BackendCloseChoiceListener />
      <Routes>
        {/* 区域截图选区页：独立 boundary，主路由错误不得白屏 overlay 窗口 */}
        <Route
          path="/screenshot-overlay"
          element={
            <IsolatedRoute routeKey="/screenshot-overlay">
              <Overlay />
            </IsolatedRoute>
          }
        />
        {/* 全屏健康提醒遮罩页：独立 boundary，与主路由错误隔离 */}
        <Route
          path="/health-overlay"
          element={
            <IsolatedRoute routeKey="/health-overlay">
              <HealthOverlay />
            </IsolatedRoute>
          }
        />
        <Route
          path="/welcome"
          element={
            <IsolatedRoute routeKey="/welcome">
              <Welcome />
            </IsolatedRoute>
          }
        />
        <Route element={<OnboardingGuard />}>
          <Route
            element={
              <WorkbenchDependencyProvider>
                <WorkbenchProjectsProvider>
                  <WorkbenchAgentHintsProvider>
                    <WorkbenchTerminalBuffersProvider>
                      <AttentionProvider loadSnapshot={attentionApi.listSnapshot}>
                        <ScratchpadAutosaveProvider>
                          {/* 运营通知协调器挂在 providers 内，可失效 Attention 并读路由前台抑制 */}
                          <MainWindowOperationalNotifications />
                          <AppShell />
                        </ScratchpadAutosaveProvider>
                      </AttentionProvider>
                    </WorkbenchTerminalBuffersProvider>
                  </WorkbenchAgentHintsProvider>
                </WorkbenchProjectsProvider>
              </WorkbenchDependencyProvider>
            }
          >
            <Route path="/" element={<ShellRoute><Home /></ShellRoute>} />
            <Route path="/attention" element={<ShellRoute><Attention /></ShellRoute>} />
            <Route path="/transfer" element={<ShellRoute><Transfer /></ShellRoute>} />
            <Route path="/prompts" element={<ShellRoute><Prompts /></ShellRoute>} />
            <Route path="/cc-history" element={<ShellRoute><CcHistory /></ShellRoute>} />
            <Route path="/workbench" element={<ShellRoute><Workbench /></ShellRoute>} />
            <Route
              path="/workbench/fleet"
              element={<Navigate to="/settings?tab=fleet" replace />}
            />
            <Route path="/scratchpad" element={<ShellRoute><Scratchpad /></ShellRoute>} />
            <Route path="/prompt-optimizer" element={<ShellRoute><PromptOptimizer /></ShellRoute>} />
            <Route path="/agent-hub" element={<ShellRoute><AgentHub /></ShellRoute>} />
            <Route
              path="/provider-manager"
              element={
                <ShellRoute>
                  <ProviderManager />
                </ShellRoute>
              }
            />
            <Route path="/claude-md" element={<Navigate to="/agent-hub" replace />} />
            <Route path="/claude-code" element={<Navigate to="/agent-hub?section=assets&target=claude" replace />} />
            <Route path="/orchestrator" element={<Navigate to="/workbench" replace />} />
            <Route path="/settings" element={<ShellRoute><Settings /></ShellRoute>} />
            <Route path="/health" element={<ShellRoute><Health /></ShellRoute>} />
            <Route path="/activity" element={<ShellRoute><ActivityStats /></ShellRoute>} />
            {isDev && DesignSystem ? (
              <Route
                path="/design-system"
                element={
                  <ShellRoute>
                    <DesignSystem />
                  </ShellRoute>
                }
              />
            ) : null}
            {isDev ? (
              <Route
                path="/__cp_route_error_fixture"
                element={
                  <ShellRoute>
                    <DevRouteErrorFixture />
                  </ShellRoute>
                }
              />
            ) : null}
            <Route path="*" element={<Navigate to="/" replace />} />
          </Route>
        </Route>
      </Routes>
    </LanDisclosureGate>
  );
}
