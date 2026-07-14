import { useCallback, useRef, useState } from 'react';
import type { ComponentType, ReactElement, ReactNode, SVGProps } from 'react';
import { useTranslation } from 'react-i18next';
import { Drawer } from '@/components/primitives';
import { formatAttentionBadgeCount } from '@/lib/attention';
import {
  BellIcon,
  BrowserIcon,
  ChevronDownIcon,
  FileIcon,
  FolderIcon,
  ForkIcon,
  HistoryIcon,
  MenuIcon,
  OrchestratorIcon,
  PromptsIcon,
  SettingsIcon,
  TerminalIcon,
  XIcon,
} from '@/lib/icons';
import {
  closeMobileNav,
  getInitialMobileNavOpen,
  getMobileWorkbenchPanelOrder,
  openMobileNav,
  selectMobilePanel,
  type MobileWorkbenchPanel,
} from '../mobileWorkbenchState';
import styles from '../MobileWorkbench.module.css';

/** 窄屏导航 Drawer 的 aria-labelledby 目标 id（稳定字符串，避免 useId SSR 漂移）。 */
const MOBILE_NAV_DRAWER_TITLE_ID = 'mobile-nav-drawer-title';

type MobileNavIcon = ComponentType<SVGProps<SVGSVGElement> & { size?: number }>;

interface MobileNavItem {
  panel: MobileWorkbenchPanel;
  icon: MobileNavIcon;
}

const MOBILE_NAV_ICONS: Record<MobileWorkbenchPanel, MobileNavIcon> = {
  projects: FolderIcon,
  attention: BellIcon,
  terminal: TerminalIcon,
  browser: BrowserIcon,
  files: FileIcon,
  git: HistoryIcon,
  worktrees: ForkIcon,
  prompt: PromptsIcon,
  automation: OrchestratorIcon,
  settings: SettingsIcon,
};

const MOBILE_NAV_ITEMS: readonly MobileNavItem[] = getMobileWorkbenchPanelOrder().map(
  (panel) => ({
    panel,
    icon: MOBILE_NAV_ICONS[panel],
  }),
);

export interface MobileWorkbenchShellProps {
  panel: MobileWorkbenchPanel;
  project: string | null;
  worktree: string | null;
  session: string | null;
  worktreeStatusDisabled?: boolean;
  worktreeStatusExpanded?: boolean;
  onWorktreeStatusClick?: () => void;
  onPanelChange: (panel: MobileWorkbenchPanel) => void;
  /** Attention 总数；0/null 不显示 badge，规则与桌面 formatAttentionBadgeCount 一致。 */
  attentionTotal?: number | null;
  children: ReactNode;
}

interface MobilePanelNavProps {
  activePanel: MobileWorkbenchPanel;
  onSelect: (panel: MobileWorkbenchPanel) => void;
  attentionBadge: string | null;
}

/**
 * MobilePanelNav（移动端工作台面板导航）
 *
 * Business Logic（为什么需要这个组件）:
 *   移动端抽屉和宽屏固定 rail 需要共享同一组面板入口，避免两个导航区域出现功能差异。
 *
 * Code Logic（这个组件做什么）:
 *   遍历 MOBILE_NAV_ITEMS 渲染 button 导航项，根据 activePanel 标记当前项，并把点击事件交给父组件。
 */
function MobilePanelNav({
  activePanel,
  onSelect,
  attentionBadge,
}: MobilePanelNavProps): ReactElement {
  const { t } = useTranslation(['workbench', 'attention']);
  const labels: Record<MobileWorkbenchPanel, string> = {
    projects: t('workbench:mobile.nav.projects'),
    attention: t('workbench:mobile.nav.attention'),
    terminal: t('workbench:mobile.nav.terminal'),
    browser: t('workbench:mobile.nav.browser'),
    files: t('workbench:mobile.nav.files'),
    git: t('workbench:mobile.nav.git'),
    worktrees: t('workbench:mobile.nav.worktrees'),
    prompt: t('workbench:mobile.nav.prompt'),
    automation: t('workbench:mobile.nav.automation'),
    settings: t('workbench:mobile.nav.settings'),
  };

  return (
    <nav className={styles.navList} aria-label={t('workbench:mobile.navAriaLabel')}>
      {MOBILE_NAV_ITEMS.map((item) => {
        const Icon = item.icon;
        const isActive = item.panel === activePanel;
        const showAttentionBadge = item.panel === 'attention' && attentionBadge !== null;

        return (
          <button
            key={item.panel}
            type="button"
            className={`${styles.navItem} ${isActive ? styles.navActive : ''}`}
            aria-current={isActive ? 'page' : undefined}
            aria-label={
              showAttentionBadge
                ? t('attention:badgeAriaLabel', { count: attentionBadge ?? '0' })
                : undefined
            }
            onClick={() => onSelect(item.panel)}
          >
            <Icon size={16} aria-hidden="true" />
            <span>{labels[item.panel]}</span>
            {showAttentionBadge ? (
              <span className={styles.mobileBadge} aria-hidden="true">
                {attentionBadge}
              </span>
            ) : null}
          </button>
        );
      })}
    </nav>
  );
}

/**
 * MobileWorkbenchShell（移动端工作台响应式外壳）
 *
 * Business Logic（为什么需要这个组件）:
 *   `/mobile` 需要在手机竖屏提供覆盖式抽屉导航，在平板/桌面宽屏提供固定 rail，给后续业务面板统一承载容器。
 *   顶部状态行还需要把当前 worktree 暴露为可选的 quick switch 入口。
 *
 * Code Logic（这个组件做什么）:
 *   管理移动抽屉 open state，使用 mobileWorkbenchState helper 切换面板/开关抽屉；
 *   窄屏导航走共享 Drawer（side=left）原语，宽屏固定 rail 仍在 Drawer 外常驻；
 *   当父组件提供 onWorktreeStatusClick 时，将 worktree pill 渲染为 dialog 触发按钮，否则保持静态状态文本。
 */
export function MobileWorkbenchShell({
  panel,
  project,
  worktree,
  session,
  worktreeStatusDisabled = false,
  worktreeStatusExpanded = false,
  onWorktreeStatusClick,
  onPanelChange,
  attentionTotal = null,
  children,
}: MobileWorkbenchShellProps): ReactElement {
  const [isNavOpen, setIsNavOpen] = useState<boolean>(() => getInitialMobileNavOpen());
  const closeButtonRef = useRef<HTMLButtonElement | null>(null);
  const { t } = useTranslation(['workbench']);
  const worktreeStatusLabel = worktree ?? t('workbench:mobile.status.worktree');
  const attentionBadge =
    attentionTotal == null ? null : formatAttentionBadgeCount(attentionTotal);

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机竖屏用户需要通过顶部按钮展开主导航抽屉。
   *
   * Code Logic（这个函数做什么）:
   *   调用 openMobileNav helper 并写入本组件的抽屉状态。
   */
  const handleOpenNav = useCallback((): void => {
    setIsNavOpen(openMobileNav());
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击关闭按钮或遮罩时需要收起覆盖式导航，避免遮挡当前面板。
   *
   * Code Logic（这个函数做什么）:
   *   调用 closeMobileNav helper 并写入本组件的抽屉状态。
   */
  const handleCloseNav = useCallback((): void => {
    setIsNavOpen(closeMobileNav());
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户从抽屉或 rail 选择面板后，应切换内容并在手机竖屏自动收起抽屉。
   *
   * Code Logic（这个函数做什么）:
   *   使用 selectMobilePanel 计算目标面板，通知父组件更新 panel，并把 drawer 状态置为关闭。
   */
  const handleSelectPanel = useCallback(
    (nextPanel: MobileWorkbenchPanel): void => {
      onPanelChange(selectMobilePanel(panel, nextPanel));
      setIsNavOpen(closeMobileNav());
    },
    [onPanelChange, panel],
  );

  return (
    <div className={styles.shell}>
      <header className={styles.topbar}>
        <button
          type="button"
          className={styles.menuButton}
          aria-label={t('workbench:mobile.openNavigation')}
          aria-expanded={isNavOpen}
          onClick={handleOpenNav}
        >
          <MenuIcon size={16} aria-hidden="true" />
        </button>
        <div className={styles.titleBlock}>
          <p className={styles.topTitle}>{t('workbench:mobile.topTitle')}</p>
          <p className={styles.topMeta}>{project ?? t('workbench:mobile.noProject')}</p>
        </div>
      </header>

      <Drawer
        open={isNavOpen}
        titleId={MOBILE_NAV_DRAWER_TITLE_ID}
        side="left"
        onClose={handleCloseNav}
        initialFocusRef={closeButtonRef}
        className={styles.drawer}
      >
        <div className={styles.drawerHeader}>
          <div className={styles.titleBlock}>
            <h2 id={MOBILE_NAV_DRAWER_TITLE_ID} className={styles.topTitle}>
              {t('workbench:mobile.topTitle')}
            </h2>
            <p className={styles.topMeta}>{project ?? t('workbench:mobile.projectFallback')}</p>
          </div>
          <button
            ref={closeButtonRef}
            type="button"
            className={styles.closeButton}
            aria-label={t('workbench:mobile.closeNavigation')}
            onClick={handleCloseNav}
          >
            <XIcon size={16} aria-hidden="true" />
          </button>
        </div>
        <MobilePanelNav
          activePanel={panel}
          onSelect={handleSelectPanel}
          attentionBadge={attentionBadge}
        />
      </Drawer>

      <aside className={styles.rail} aria-label={t('workbench:mobile.railAriaLabel')}>
        <div className={styles.railHeader}>
          <p className={styles.topTitle}>{t('workbench:mobile.topTitle')}</p>
          <p className={styles.topMeta}>{project ?? t('workbench:mobile.noProject')}</p>
        </div>
        <MobilePanelNav
          activePanel={panel}
          onSelect={handleSelectPanel}
          attentionBadge={attentionBadge}
        />
      </aside>

      <main className={styles.content}>
        <div className={styles.statusRow} aria-label={t('workbench:mobile.statusAriaLabel')}>
          <span className={styles.statusPill}>{project ?? t('workbench:mobile.status.project')}</span>
          {onWorktreeStatusClick ? (
            <button
              type="button"
              className={`${styles.statusPill} ${styles.statusPillButton}`}
              disabled={worktreeStatusDisabled}
              aria-haspopup="dialog"
              aria-expanded={worktreeStatusExpanded}
              onClick={onWorktreeStatusClick}
            >
              <span className={styles.statusPillText}>{worktreeStatusLabel}</span>
              <ChevronDownIcon
                size={14}
                className={styles.statusPillIcon}
                aria-hidden="true"
              />
            </button>
          ) : (
            <span className={styles.statusPill}>{worktreeStatusLabel}</span>
          )}
          <span className={styles.statusPill}>{session ?? t('workbench:mobile.status.session')}</span>
        </div>
        {children}
      </main>
    </div>
  );
}
