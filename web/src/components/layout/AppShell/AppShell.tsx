/**
 * AppShell（整个应用外壳）
 *
 * Business Logic（为什么需要这个组件）:
 *   cc-partner 是一个三端（macOS / Windows / Linux）桌面工具，
 *   Web 端需要提供侧边导航 + 主内容区的基本布局骨架，
 *   窗口标题栏由 PyQt6 原生提供，无需 Web 端自绘。
 *   侧边栏 footer 区域集中展示版本号、版本行最右的游戏与移动端访问按钮组、语言/主题/设置；
 *   设置固定在 footer，避免小屏滚动才能看到。
 *   主导航按 Explore/Work/Knowledge/System 分组，短窗口下可滚动；
 *   Workbench 入口是 Work 组内项目列表，不占独立主导航项。
 *
 * Code Logic（这个组件做什么）:
 *   - 全屏 flex 布局：左侧 Sidebar（240px）+ 右侧 main 区域
 *   - Sidebar 内包含 Logo、分组导航（section + 非聚焦 group label）、
 *     Work 组内 ProjectRail、footer（版本号 + 版本行最右的游戏与移动端访问按钮组 + 语言/主题/设置齿轮）
 *   - 设置入口为 footer NavLink(`/settings`)，System 组保留健康提醒、活动统计与 Provider 管理
 *   - 手机访问入口经共享 Dialog 呈现 MobileAccessCard（Escape/backdrop/焦点恢复由 Dialog 合同处理）
 *   - 右侧 main 区域是 <outlet /> 出口，由 React Router 注入子页面，
 *     main 自带 overflow: auto 实现独立滚动
 *   - 今日标语挂在外壳顶栏，按整窗水平居中，不跟工作台标题行剩余空档对齐
 *
 *   注意：本组件是 <Outlet /> 容器，children 不直接使用。
 */
import { lazy, Suspense, useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useWorkbenchWindowRole } from '@/hooks/useWorkbenchWindowRole';
import { useTheme } from '@/hooks/useTheme';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
import { syncWorkbenchWindowTitle } from '@/lib/workbenchWindowTitle';
import { NavLink, Outlet, useLocation } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import {
  HomeIcon,
  EditIcon,
  TransferIcon,
  PromptsIcon,
  HistoryIcon,
  ScratchpadIcon,
  ClaudeMdIcon,
  SettingsIcon,
  HealthIcon,
  ActivityIcon,
  ProviderManagerIcon,
  AlertIcon,
  TokenIcon,
  SmartphoneIcon,
  XIcon,
  GameIcon,
} from '../../../lib/icons';
import { useAppVersion } from '../../../hooks/useAppVersion';
import { useAttention } from '../../../hooks/useAttention';
import { formatAttentionBadgeCount } from '../../../lib/attention';
import { Sidebar } from '../Sidebar';
import { NavItem } from '../NavItem';
import { ThemeToggle } from '../ThemeToggle';
import { LanguageSwitcher } from '../LanguageSwitcher';
import { BatteryModeToggle } from '../BatteryModeToggle';
import { BatteryWorkbenchScrim } from '../BatteryWorkbenchScrim';
import { BatteryCreditToast } from '../BatteryCreditToast';
import { useBattery } from '@/hooks/useBattery';
import { formatBatteryTime } from '@/lib/batteryTime';
import { MobileAccessCard } from '@/components/domain/MobileAccessCard';
import { PermissionStatusBadge } from '@/components/domain/PermissionStatusBadge';
import { WorkbenchProjectRail } from '@/components/domain/WorkbenchProjectRail';
import { Dialog } from '@/components/primitives';
import { WorkbenchBanner } from '@/pages/Workbench/views/WorkbenchBanner';

const GameHubDialog = lazy(async () => {
  const module = await import('@/components/domain/GameHubDialog');
  return { default: module.GameHubDialog };
});
// 应用内健康 toast 已停用（改用系统通知 HealthReminderListener + 全屏遮罩 HealthOverlay），
// 组件代码保留以便恢复。先测试系统级提醒是否够用。
// import ReminderToast from '@/pages/Health/ReminderToast';
// import WaterToast from '@/pages/Health/WaterToast';
import appIconUrl from '@/assets/app-icon.png';
import styles from './AppShell.module.css';

export interface AppShellProps {
  /** 路由出口占位（一般由 react-router 注入 <Outlet />，可显式覆盖） */
  children?: React.ReactNode;
}

const MOBILE_ACCESS_DIALOG_ID = 'app-shell-mobile-access-dialog';
const MOBILE_ACCESS_TITLE_ID = 'app-shell-mobile-access-title';

const NAV_GROUP_IDS = {
  explore: 'nav-group-explore',
  work: 'nav-group-work',
  knowledge: 'nav-group-knowledge',
  system: 'nav-group-system',
} as const;

/**
 * Business Logic（为什么需要这个函数）:
 *   侧栏导航按任务域分组后，每组需要统一的 section 外壳与不可聚焦标题。
 *
 * Code Logic（这个函数做什么）:
 *   渲染带 aria-labelledby 的 section、非交互 group label，以及组内 children。
 */
function NavGroup({
  id,
  label,
  children,
}: {
  id: string;
  label: string;
  children: ReactNode;
}) {
  return (
    <section className={styles.navGroup} aria-labelledby={id}>
      <div id={id} className={styles.navGroupLabel}>
        {label}
      </div>
      {children}
    </section>
  );
}

export function AppShell({ children }: AppShellProps) {
  // 版本号以后端 __init__.py 的 __version__ 为唯一权威来源，通过 useAppVersion
  // 从 /api/version 动态获取，前端不再硬编码，避免发版漏改导致版本不一致。
  const version = useAppVersion();
  const { snapshot: attentionSnapshot } = useAttention();
  const attentionBadge = formatAttentionBadgeCount(
    attentionSnapshot?.counts.unreadTotal ?? 0,
  );
  // 传入命名空间数组,让 react-i18next v17 的 t() 类型校验 ns:key 形式
  // (无参时 t() 只接受 defaultNS 即 common 的扁平 key,'nav:*' 会类型报错)
  const { t } = useTranslation(['common', 'nav', 'settings', 'wordgame']);
  const { t: tBattery } = useTranslation('battery');
  const [mobileAccessOpen, setMobileAccessOpen] = useState<boolean>(false);
  const [gameHubOpen, setGameHubOpen] = useState<boolean>(false);
  const location = useLocation();
  const {
    snapshot: batterySnapshot,
    toast: batteryToast,
    setMode: setBatteryMode,
    dismissToast: dismissBatteryToast,
  } = useBattery();
  const batteryDepleted =
    batterySnapshot?.mode === 'charging' && (batterySnapshot.remainingMs ?? 0) <= 0;
  const showWorkbenchScrim = batteryDepleted && location.pathname.startsWith('/workbench');
  const handleBatteryToggle = useCallback((next: 'charging' | 'unlimited'): void => {
    void setBatteryMode(next);
  }, [setBatteryMode]);
  const batteryRemainingLabel = formatBatteryTime(
    batterySnapshot?.remainingMs ?? 0,
    tBattery,
  );
  const mobileAccessButtonRef = useRef<HTMLButtonElement | null>(null);
  const appName = t('common:app.name');
  const { role } = useWorkbenchWindowRole();
  const { activeProject, remoteWriteDisabled } = useWorkbenchProjects();
  // 卫星窗不渲染 ThemeToggle，但仍须挂载 useTheme 才能写 data-theme 并跨窗同步。
  useTheme();
  const isSatellite = role === 'satellite';
  const bannerDeviceId =
    location.pathname.startsWith('/workbench') && activeProject?.kind === 'remote'
      ? activeProject.deviceId
      : undefined;

  useEffect(() => {
    void syncWorkbenchWindowTitle(
      (title) => getCurrentWindow().setTitle(title),
      activeProject?.name ?? null,
    );
  }, [activeProject?.name]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户打开移动端 Workbench 访问对话框后，需要能通过关闭按钮、Escape 或 backdrop 收起。
   *
   * Code Logic（这个函数做什么）:
   *   将 AppShell 内部的移动访问对话框状态置为关闭，供按钮与 Dialog onClose 复用。
   */
  const closeMobileAccess = useCallback((): void => {
    setMobileAccessOpen(false);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   侧栏 footer 的手机按钮需要在不跳转页面的情况下打开或关闭移动访问对话框。
   *
   * Code Logic（这个函数做什么）:
   *   使用函数式 setState 基于前一状态取反，避免依赖当前渲染闭包里的状态值。
   */
  const toggleMobileAccess = useCallback((): void => {
    setMobileAccessOpen((open) => !open);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   版本行最右的 game 图标按钮打开大厅；关闭时回到未打开态。
   *
   * Code Logic（这个函数做什么）:
   *   打开 / 关闭 GameHub Dialog。
   */
  const openGameHub = useCallback((): void => {
    setGameHubOpen(true);
  }, []);

  const closeGameHub = useCallback((): void => {
    setGameHubOpen(false);
  }, []);

  return (
    <div
      className={styles.layout}
      data-testid={isSatellite ? 'workbench-satellite-shell' : undefined}
    >
      <Sidebar
        footer={
          isSatellite ? (
          <div className={styles.footer} data-testid="battery-satellite-footer">
            <div className={styles.footerToggle}>
              <div className={styles.footerIconGroup}>
                <BatteryModeToggle
                  snapshot={batterySnapshot}
                  onToggle={handleBatteryToggle}
                />
                <span className={styles.satelliteRemaining} aria-live="polite">
                  {batteryRemainingLabel}
                </span>
              </div>
            </div>
          </div>
          ) : (
          <div className={styles.footer}>
            <span className={styles.footerVersionRow}>
              <span className={styles.footerVersion}>{`v${version ?? '—'}`}</span>
              <span className={styles.footerIconGroup}>
                <button
                  type="button"
                  className={styles.footerIconButton}
                  onClick={openGameHub}
                  aria-haspopup="dialog"
                  aria-expanded={gameHubOpen}
                  aria-label={t('wordgame:gameButtonTitle')}
                  title={t('wordgame:gameButtonTitle')}
                >
                  <GameIcon size={14} />
                </button>
                <button
                  ref={mobileAccessButtonRef}
                  type="button"
                  className={styles.footerIconButton}
                  onClick={toggleMobileAccess}
                  aria-label={t('settings:mobileAccess.buttonLabel')}
                  aria-haspopup="dialog"
                  aria-expanded={mobileAccessOpen}
                  aria-controls={MOBILE_ACCESS_DIALOG_ID}
                  title={t('settings:mobileAccess.buttonTitle')}
                >
                  <SmartphoneIcon size={14} />
                </button>
              </span>
            </span>
            <div className={styles.footerToggle}>
              <LanguageSwitcher />
              <div className={styles.footerIconGroup}>
                <BatteryModeToggle
                  snapshot={batterySnapshot}
                  onToggle={handleBatteryToggle}
                />
                <ThemeToggle />
                <NavLink
                  to="/settings"
                  className={({ isActive }) =>
                    isActive
                      ? `${styles.footerIconButton} ${styles.footerIconButtonActive}`
                      : styles.footerIconButton
                  }
                  aria-label={t('nav:settings')}
                  title={t('nav:settings')}
                >
                  <SettingsIcon size={14} />
                </NavLink>
              </div>
            </div>
          </div>
          )
        }
      >
        <div className={styles.logo}>
          <img className={styles.logoMark} src={appIconUrl} alt="" aria-hidden="true" />
          <span className={styles.logoText}>{appName}</span>
        </div>
        {isSatellite ? (
          <nav className={styles.navList} aria-label={t('nav:primaryNav')}>
            <NavGroup id={NAV_GROUP_IDS.work} label={t('nav:groups.work')}>
              <WorkbenchProjectRail />
            </NavGroup>
          </nav>
        ) : (
          <>
            <nav className={styles.navList} aria-label={t('nav:primaryNav')}>
              <NavGroup id={NAV_GROUP_IDS.explore} label={t('nav:groups.explore')}>
                <NavItem to="/" label={t('nav:home')} icon={<HomeIcon />} />
              </NavGroup>
              <NavGroup id={NAV_GROUP_IDS.work} label={t('nav:groups.work')}>
                <NavItem
                  to="/attention"
                  label={t('nav:attention')}
                  icon={<AlertIcon />}
                  badge={attentionBadge ?? undefined}
                />
                <NavItem to="/transfer" label={t('nav:transfer')} icon={<TransferIcon />} />
                <WorkbenchProjectRail />
              </NavGroup>
              <NavGroup id={NAV_GROUP_IDS.knowledge} label={t('nav:groups.knowledge')}>
                <NavItem to="/prompts" label={t('nav:prompts')} icon={<PromptsIcon />} />
                <NavItem to="/cc-history" label={t('nav:ccHistory')} icon={<HistoryIcon />} />
                <NavItem to="/scratchpad" label={t('nav:scratchpad')} icon={<ScratchpadIcon />} />
                <NavItem to="/prompt-optimizer" label={t('nav:promptOptimizer')} icon={<EditIcon />} />
                <NavItem to="/agent-hub" label={t('nav:agentHub')} icon={<ClaudeMdIcon />} />
              </NavGroup>
              <NavGroup id={NAV_GROUP_IDS.system} label={t('nav:groups.system')}>
                <NavItem to="/health" label={t('nav:health')} icon={<HealthIcon />} />
                <NavItem to="/activity" label={t('nav:activity')} icon={<ActivityIcon />} />
                <NavItem to="/token-stats" label={t('nav:tokenStats')} icon={<TokenIcon />} />
                <NavItem
                  to="/provider-manager"
                  label={t('nav:providerManager')}
                  icon={<ProviderManagerIcon />}
                />
              </NavGroup>
            </nav>
            <PermissionStatusBadge />
          </>
        )}
      </Sidebar>
      <div className={styles.bannerSlot} data-testid="app-banner-slot">
        <WorkbenchBanner
          deviceId={bannerDeviceId}
          remoteWriteDisabled={Boolean(bannerDeviceId) && remoteWriteDisabled}
        />
      </div>
      <main className={styles.main}>
        {children ?? <Outlet />}
        <BatteryWorkbenchScrim visible={showWorkbenchScrim} onOpenGame={openGameHub} />
      </main>
      <BatteryCreditToast toast={batteryToast} onDismiss={dismissBatteryToast} />
      <Dialog
        open={mobileAccessOpen}
        titleId={MOBILE_ACCESS_TITLE_ID}
        onClose={closeMobileAccess}
        className={styles.mobileAccessDialog}
      >
        <div id={MOBILE_ACCESS_DIALOG_ID} className={styles.mobileAccessDialogBody}>
          <h2 id={MOBILE_ACCESS_TITLE_ID} className="sr-only">
            {t('settings:mobileAccess.dialogLabel')}
          </h2>
          <button
            type="button"
            className={styles.mobileAccessClose}
            onClick={closeMobileAccess}
            aria-label={t('settings:mobileAccess.close')}
            title={t('settings:mobileAccess.close')}
          >
            <XIcon size={14} />
          </button>
          <MobileAccessCard compact className={styles.mobileAccessCard} />
        </div>
      </Dialog>
      {gameHubOpen ? (
        <Suspense fallback={null}>
          <GameHubDialog open={gameHubOpen} onClose={closeGameHub} />
        </Suspense>
      ) : null}
      {/* 应用内健康 toast 已停用（改用系统通知 + 全屏遮罩），代码保留以便恢复（先测试）：
          <ReminderToast />
          <WaterToast /> */}
    </div>
  );
}
