import { useCallback, useState } from 'react';
import type { ComponentType, ReactElement, ReactNode, SVGProps } from 'react';
import { useTranslation } from 'react-i18next';
import {
  FileIcon,
  FolderIcon,
  ForkIcon,
  HistoryIcon,
  MenuIcon,
  PromptsIcon,
  SettingsIcon,
  TerminalIcon,
  XIcon,
} from '@/lib/icons';
import {
  closeMobileNav,
  getInitialMobileNavOpen,
  openMobileNav,
  selectMobilePanel,
  type MobileWorkbenchPanel,
} from '../mobileWorkbenchState';
import styles from '../MobileWorkbench.module.css';

type MobileNavIcon = ComponentType<SVGProps<SVGSVGElement> & { size?: number }>;

interface MobileNavItem {
  panel: MobileWorkbenchPanel;
  icon: MobileNavIcon;
}

const MOBILE_NAV_ITEMS: readonly MobileNavItem[] = [
  { panel: 'projects', icon: FolderIcon },
  { panel: 'terminal', icon: TerminalIcon },
  { panel: 'files', icon: FileIcon },
  { panel: 'git', icon: HistoryIcon },
  { panel: 'worktrees', icon: ForkIcon },
  { panel: 'prompt', icon: PromptsIcon },
  { panel: 'settings', icon: SettingsIcon },
];

export interface MobileWorkbenchShellProps {
  panel: MobileWorkbenchPanel;
  project: string | null;
  worktree: string | null;
  session: string | null;
  onPanelChange: (panel: MobileWorkbenchPanel) => void;
  children: ReactNode;
}

interface MobilePanelNavProps {
  activePanel: MobileWorkbenchPanel;
  onSelect: (panel: MobileWorkbenchPanel) => void;
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
function MobilePanelNav({ activePanel, onSelect }: MobilePanelNavProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const labels: Record<MobileWorkbenchPanel, string> = {
    projects: t('workbench:mobile.nav.projects'),
    terminal: t('workbench:mobile.nav.terminal'),
    files: t('workbench:mobile.nav.files'),
    git: t('workbench:mobile.nav.git'),
    worktrees: t('workbench:mobile.nav.worktrees'),
    prompt: t('workbench:mobile.nav.prompt'),
    settings: t('workbench:mobile.nav.settings'),
  };

  return (
    <nav className={styles.navList} aria-label={t('workbench:mobile.navAriaLabel')}>
      {MOBILE_NAV_ITEMS.map((item) => {
        const Icon = item.icon;
        const isActive = item.panel === activePanel;

        return (
          <button
            key={item.panel}
            type="button"
            className={`${styles.navItem} ${isActive ? styles.navActive : ''}`}
            aria-current={isActive ? 'page' : undefined}
            onClick={() => onSelect(item.panel)}
          >
            <Icon size={16} aria-hidden="true" />
            <span>{labels[item.panel]}</span>
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
 *
 * Code Logic（这个组件做什么）:
 *   管理移动抽屉 open state，使用 mobileWorkbenchState helper 切换面板/开关抽屉，并渲染 topbar、drawer、rail 与内容区。
 */
export function MobileWorkbenchShell({
  panel,
  project,
  worktree,
  session,
  onPanelChange,
  children,
}: MobileWorkbenchShellProps): ReactElement {
  const [isNavOpen, setIsNavOpen] = useState<boolean>(() => getInitialMobileNavOpen());
  const { t } = useTranslation(['workbench']);

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

      {isNavOpen ? (
        <>
          <button
            type="button"
            className={styles.backdrop}
            aria-label={t('workbench:mobile.closeNavigation')}
            onClick={handleCloseNav}
          />
          <aside className={styles.drawer} aria-label={t('workbench:mobile.drawerAriaLabel')}>
            <div className={styles.drawerHeader}>
              <div className={styles.titleBlock}>
                <p className={styles.topTitle}>{t('workbench:mobile.topTitle')}</p>
                <p className={styles.topMeta}>{project ?? t('workbench:mobile.projectFallback')}</p>
              </div>
              <button
                type="button"
                className={styles.closeButton}
                aria-label={t('workbench:mobile.closeNavigation')}
                onClick={handleCloseNav}
              >
                <XIcon size={16} aria-hidden="true" />
              </button>
            </div>
            <MobilePanelNav activePanel={panel} onSelect={handleSelectPanel} />
          </aside>
        </>
      ) : null}

      <aside className={styles.rail} aria-label={t('workbench:mobile.railAriaLabel')}>
        <div className={styles.railHeader}>
          <p className={styles.topTitle}>{t('workbench:mobile.topTitle')}</p>
          <p className={styles.topMeta}>{project ?? t('workbench:mobile.noProject')}</p>
        </div>
        <MobilePanelNav activePanel={panel} onSelect={handleSelectPanel} />
      </aside>

      <main className={styles.content}>
        <div className={styles.statusRow} aria-label={t('workbench:mobile.statusAriaLabel')}>
          <span className={styles.statusPill}>{project ?? t('workbench:mobile.status.project')}</span>
          <span className={styles.statusPill}>
            {worktree ?? t('workbench:mobile.status.worktree')}
          </span>
          <span className={styles.statusPill}>{session ?? t('workbench:mobile.status.session')}</span>
        </div>
        {children}
      </main>
    </div>
  );
}
