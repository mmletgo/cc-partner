import { useCallback, useState } from 'react';
import type { ComponentType, ReactElement, ReactNode, SVGProps } from 'react';
import {
  FileIcon,
  FolderIcon,
  ForkIcon,
  HistoryIcon,
  PromptsIcon,
  SettingsIcon,
  TerminalIcon,
  XIcon,
} from '@/lib/icons';
import {
  closeMobileNav,
  openMobileNav,
  selectMobilePanel,
  type MobileWorkbenchPanel,
} from '../mobileWorkbenchState';
import styles from '../MobileWorkbench.module.css';

type MobileNavIcon = ComponentType<SVGProps<SVGSVGElement> & { size?: number }>;

interface MobileNavItem {
  panel: MobileWorkbenchPanel;
  label: string;
  icon: MobileNavIcon;
}

const MOBILE_NAV_ITEMS: readonly MobileNavItem[] = [
  { panel: 'projects', label: '项目', icon: FolderIcon },
  { panel: 'terminal', label: '终端', icon: TerminalIcon },
  { panel: 'files', label: '文件', icon: FileIcon },
  { panel: 'git', label: 'Git', icon: HistoryIcon },
  { panel: 'worktrees', label: 'Worktrees', icon: ForkIcon },
  { panel: 'prompt', label: 'Prompt', icon: PromptsIcon },
  { panel: 'settings', label: '设置', icon: SettingsIcon },
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
  return (
    <nav className={styles.navList} aria-label="Mobile Workbench panels">
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
            <span>{item.label}</span>
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
  const [isNavOpen, setIsNavOpen] = useState<boolean>(false);

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
          aria-label="打开导航"
          aria-expanded={isNavOpen}
          onClick={handleOpenNav}
        >
          <span aria-hidden="true">☰</span>
        </button>
        <div className={styles.titleBlock}>
          <p className={styles.topTitle}>Workbench</p>
          <p className={styles.topMeta}>{project ?? '未选择项目'}</p>
        </div>
      </header>

      {isNavOpen ? (
        <>
          <button
            type="button"
            className={styles.backdrop}
            aria-label="关闭导航"
            onClick={handleCloseNav}
          />
          <aside className={styles.drawer} aria-label="移动端导航">
            <div className={styles.drawerHeader}>
              <div className={styles.titleBlock}>
                <p className={styles.topTitle}>Workbench</p>
                <p className={styles.topMeta}>{project ?? '项目'}</p>
              </div>
              <button
                type="button"
                className={styles.closeButton}
                aria-label="关闭导航"
                onClick={handleCloseNav}
              >
                <XIcon size={16} aria-hidden="true" />
              </button>
            </div>
            <MobilePanelNav activePanel={panel} onSelect={handleSelectPanel} />
          </aside>
        </>
      ) : null}

      <aside className={styles.rail} aria-label="宽屏导航">
        <div className={styles.railHeader}>
          <p className={styles.topTitle}>Workbench</p>
          <p className={styles.topMeta}>{project ?? '未选择项目'}</p>
        </div>
        <MobilePanelNav activePanel={panel} onSelect={handleSelectPanel} />
      </aside>

      <main className={styles.content}>
        <div className={styles.statusRow} aria-label="当前工作台状态">
          <span className={styles.statusPill}>{project ?? '项目'}</span>
          <span className={styles.statusPill}>{worktree ?? 'worktree'}</span>
          <span className={styles.statusPill}>{session ?? 'session'}</span>
        </div>
        {children}
      </main>
    </div>
  );
}
