/**
 * AppShell（整个应用外壳）
 *
 * Business Logic（为什么需要这个组件）:
 *   cc-partner 是一个三端（macOS / Windows / Linux）桌面工具，
 *   Web 端需要提供侧边导航 + 主内容区的基本布局骨架，
 *   窗口标题栏由 PyQt6 原生提供，无需 Web 端自绘。
 *   侧边栏 footer 区域集中展示版本号、语言/主题切换和移动端访问入口。
 *
 * Code Logic（这个组件做什么）:
 *   - 全屏 flex 布局：左侧 Sidebar（240px）+ 右侧 main 区域
 *   - Sidebar 内包含 Logo、导航项、项目文件夹入口、footer（版本号 + 语言/主题切换 + 手机访问按钮）
 *   - 右侧 main 区域是 <Outlet /> 出口，由 React Router 注入子页面，
 *     main 自带 overflow: auto 实现独立滚动
 *
 *   注意：本组件是 <Outlet /> 容器，children 不直接使用。
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { Outlet } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import {
  HomeIcon,
  EditIcon,
  TransferIcon,
  PromptsIcon,
  HistoryIcon,
  ScratchpadIcon,
  ClaudeMdIcon,
  TerminalIcon,
  DevicesIcon,
  SettingsIcon,
  HealthIcon,
  AlertIcon,
  SmartphoneIcon,
  XIcon,
} from '../../../lib/icons';
import { useAppVersion } from '../../../hooks/useAppVersion';
import { useAttention } from '../../../hooks/useAttention';
import { formatAttentionBadgeCount } from '../../../lib/attention';
import { Sidebar } from '../Sidebar';
import { NavItem } from '../NavItem';
import { ThemeToggle } from '../ThemeToggle';
import { LanguageSwitcher } from '../LanguageSwitcher';
import { MobileAccessCard, PermissionStatusBadge, WorkbenchProjectRail } from '@/components/domain';
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

export function AppShell({ children }: AppShellProps) {
  // 版本号以后端 __init__.py 的 __version__ 为唯一权威来源，通过 useAppVersion
  // 从 /api/version 动态获取，前端不再硬编码，避免发版漏改导致版本不一致。
  const version = useAppVersion();
  const { snapshot: attentionSnapshot } = useAttention();
  const attentionBadge = formatAttentionBadgeCount(attentionSnapshot?.counts.total ?? 0);
  // 传入命名空间数组,让 react-i18next v17 的 t() 类型校验 ns:key 形式
  // (无参时 t() 只接受 defaultNS 即 common 的扁平 key,'nav:*' 会类型报错)
  const { t } = useTranslation(['common', 'nav', 'settings']);
  const [mobileAccessOpen, setMobileAccessOpen] = useState<boolean>(false);
  const mobileAccessButtonRef = useRef<HTMLButtonElement | null>(null);
  const mobileAccessDialogRef = useRef<HTMLDivElement | null>(null);
  const appName = t('common:app.name');

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户打开移动端 Workbench 访问弹层后，需要能通过按钮、Escape 或外部点击快速收起。
   *
   * Code Logic（这个函数做什么）:
   *   将 AppShell 内部的移动访问弹层状态置为关闭，供多个事件处理入口复用。
   */
  const closeMobileAccess = useCallback((): void => {
    setMobileAccessOpen(false);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   侧栏 footer 的手机按钮需要在不跳转页面的情况下打开或关闭移动访问弹层。
   *
   * Code Logic（这个函数做什么）:
   *   使用函数式 setState 基于前一状态取反，避免依赖当前渲染闭包里的状态值。
   */
  const toggleMobileAccess = useCallback((): void => {
    setMobileAccessOpen((open) => !open);
  }, []);

  useEffect(() => {
    if (!mobileAccessOpen) return undefined;

    /**
     * Business Logic（为什么需要这个函数）:
     *   弹层打开后，键盘用户需要能用 Escape 退出当前移动访问信息面板。
     *
     * Code Logic（这个函数做什么）:
     *   监听全局 keydown；当按键为 Escape 时关闭弹层。
     */
    const handleKeyDown = (event: KeyboardEvent): void => {
      if (event.key === 'Escape') closeMobileAccess();
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   弹层打开后，鼠标或触控用户点击弹层外部应回到普通侧栏状态。
     *
     * Code Logic（这个函数做什么）:
     *   捕获 pointerdown，若事件目标不在弹层或触发按钮内，则关闭弹层。
     */
    const handlePointerDown = (event: PointerEvent): void => {
      const { target } = event;
      if (!(target instanceof Node)) return;
      if (mobileAccessDialogRef.current?.contains(target)) return;
      if (mobileAccessButtonRef.current?.contains(target)) return;
      closeMobileAccess();
    };

    document.addEventListener('keydown', handleKeyDown);
    document.addEventListener('pointerdown', handlePointerDown, true);
    return () => {
      document.removeEventListener('keydown', handleKeyDown);
      document.removeEventListener('pointerdown', handlePointerDown, true);
    };
  }, [closeMobileAccess, mobileAccessOpen]);

  return (
    <div className={styles.layout}>
      <Sidebar
        footer={
          <div className={styles.footer}>
            <span className={styles.footerVersion}>v{version ?? '—'}</span>
            <span>{appName}</span>
            <div className={styles.footerToggle}>
              <LanguageSwitcher />
              <div className={styles.footerIconGroup}>
                <ThemeToggle />
                <button
                  ref={mobileAccessButtonRef}
                  type="button"
                  className={styles.mobileAccessButton}
                  onClick={toggleMobileAccess}
                  aria-label={t('settings:mobileAccess.buttonLabel')}
                  aria-haspopup="dialog"
                  aria-expanded={mobileAccessOpen}
                  aria-controls={MOBILE_ACCESS_DIALOG_ID}
                  title={t('settings:mobileAccess.buttonTitle')}
                >
                  <SmartphoneIcon size={14} />
                </button>
              </div>
            </div>
            {mobileAccessOpen ? (
              <div
                ref={mobileAccessDialogRef}
                id={MOBILE_ACCESS_DIALOG_ID}
                className={styles.mobileAccessPopover}
                role="dialog"
                aria-label={t('settings:mobileAccess.dialogLabel')}
              >
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
            ) : null}
          </div>
        }
      >
        <div className={styles.logo}>
          <img className={styles.logoMark} src={appIconUrl} alt="" aria-hidden="true" />
          <span className={styles.logoText}>{appName}</span>
        </div>
        <nav className={styles.navList} aria-label="primary">
          <NavItem to="/" label={t('nav:home')} icon={<HomeIcon />} />
          <NavItem
            to="/attention"
            label={t('nav:attention')}
            icon={<AlertIcon />}
            badge={attentionBadge ?? undefined}
          />
          <NavItem to="/prompts" label={t('nav:prompts')} icon={<PromptsIcon />} />
          <NavItem to="/cc-history" label={t('nav:ccHistory')} icon={<HistoryIcon />} />
          <NavItem to="/scratchpad" label={t('nav:scratchpad')} icon={<ScratchpadIcon />} />
          <NavItem to="/prompt-optimizer" label={t('nav:promptOptimizer')} icon={<EditIcon />} />
          <NavItem to="/transfer" label={t('nav:transfer')} icon={<TransferIcon />} />
          <NavItem to="/claude-md" label={t('nav:claudeMd')} icon={<ClaudeMdIcon />} />
          <NavItem to="/claude-code" label={t('nav:claudeCode')} icon={<TerminalIcon />} />
          <NavItem to="/devices" label={t('nav:devices')} icon={<DevicesIcon />} />
          <NavItem to="/health" label={t('nav:health')} icon={<HealthIcon />} />
          <NavItem to="/settings" label={t('nav:settings')} icon={<SettingsIcon />} />
        </nav>
        <WorkbenchProjectRail />
        <PermissionStatusBadge />
      </Sidebar>
      <main className={styles.main}>{children ?? <Outlet />}</main>
      {/* 应用内健康 toast 已停用（改用系统通知 + 全屏遮罩），代码保留以便恢复（先测试）：
          <ReminderToast />
          <WaterToast /> */}
    </div>
  );
}
