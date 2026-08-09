import { useCallback, useEffect, useRef, useState } from 'react';
import type { ComponentType, CSSProperties, ReactElement, ReactNode, SVGProps } from 'react';
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
  ProviderManagerIcon,
  PromptsIcon,
  SettingsIcon,
  TerminalIcon,
  XIcon,
} from '@/lib/icons';
import {
  closeMobileNav,
  computeMobileViewportLayoutHints,
  getInitialMobileNavOpen,
  getMobileWorkbenchNavGroups,
  openMobileNav,
  selectMobilePanel,
  type MobileConnectionState,
  type MobileWorkbenchNavGroupId,
  type MobileWorkbenchPanel,
} from '../mobileWorkbenchState';
import styles from '../MobileWorkbench.module.css';

/** 窄屏导航 Drawer 的 aria-labelledby 目标 id（稳定字符串，避免 useId SSR 漂移）。 */
const MOBILE_NAV_DRAWER_TITLE_ID = 'mobile-nav-drawer-title';

type MobileNavIcon = ComponentType<SVGProps<SVGSVGElement> & { size?: number }>;

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
  provider: ProviderManagerIcon,
};

const MOBILE_NAV_GROUP_TITLE_IDS: Record<MobileWorkbenchNavGroupId, string> = {
  projects: 'mobile-nav-group-projects',
  attention: 'mobile-nav-group-attention',
  work: 'mobile-nav-group-work',
  automation: 'mobile-nav-group-automation',
  more: 'mobile-nav-group-more',
};

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
  /** 弱网连接态；offline/reconnecting 时展示缓存时间。 */
  connectionState?: MobileConnectionState | null;
  /** 可展示的缓存起点 epoch ms。 */
  connectionCachedAt?: number | null;
  children: ReactNode;
}

interface MobilePanelNavProps {
  activePanel: MobileWorkbenchPanel;
  onSelect: (panel: MobileWorkbenchPanel) => void;
  attentionBadge: string | null;
}

/**
 * MobilePanelNav（移动端工作台分组导航）
 *
 * Business Logic（为什么需要这个组件）:
 *   移动端抽屉和宽屏固定 rail 需要按任务分组共享同一组面板入口，降低扁平十项的导航负担。
 *
 * Code Logic（这个组件做什么）:
 *   遍历 getMobileWorkbenchNavGroups 渲染 section + panel 按钮；根据 activePanel 标记当前项。
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
    provider: t('workbench:mobile.nav.provider'),
  };
  const groupLabels: Record<MobileWorkbenchNavGroupId, string> = {
    projects: t('workbench:mobile.navGroups.projects'),
    attention: t('workbench:mobile.navGroups.attention'),
    work: t('workbench:mobile.navGroups.work'),
    automation: t('workbench:mobile.navGroups.automation'),
    more: t('workbench:mobile.navGroups.more'),
  };

  return (
    <nav className={styles.navList} aria-label={t('workbench:mobile.navAriaLabel')}>
      {getMobileWorkbenchNavGroups().map((group) => {
        const titleId = MOBILE_NAV_GROUP_TITLE_IDS[group.id];
        return (
          <section
            key={group.id}
            className={styles.navGroup}
            aria-labelledby={titleId}
            data-nav-group={group.id}
          >
            <div id={titleId} className={styles.navGroupLabel}>
              {groupLabels[group.id]}
            </div>
            <div className={styles.navGroupItems}>
              {group.panels.map((panel) => {
                const Icon = MOBILE_NAV_ICONS[panel];
                const isActive = panel === activePanel;
                const showAttentionBadge = panel === 'attention' && attentionBadge !== null;

                return (
                  <button
                    key={panel}
                    type="button"
                    className={`${styles.navItem} ${isActive ? styles.navActive : ''}`}
                    aria-current={isActive ? 'page' : undefined}
                    data-panel={panel}
                    aria-label={
                      showAttentionBadge
                        ? t('attention:badgeAriaLabel', { count: attentionBadge ?? '0' })
                        : undefined
                    }
                    onClick={() => onSelect(panel)}
                  >
                    <Icon size={16} aria-hidden="true" />
                    <span>{labels[panel]}</span>
                    {showAttentionBadge ? (
                      <span className={styles.mobileBadge} aria-hidden="true">
                        {attentionBadge}
                      </span>
                    ) : null}
                  </button>
                );
              })}
            </div>
          </section>
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
 *   顶部状态行还需要把当前 worktree 暴露为可选的 quick switch 入口；软键盘弹出时用 visualViewport 压缩高度。
 *
 * Code Logic（这个组件做什么）:
 *   管理移动抽屉 open state，按分组渲染导航；监听 visualViewport 写入 shell CSS 变量；
 *   窄屏导航走共享 Drawer（side=left）原语，宽屏固定 rail 仍在 Drawer 外常驻；不引入 bottom nav。
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
  connectionState = null,
  connectionCachedAt = null,
  children,
}: MobileWorkbenchShellProps): ReactElement {
  const [isNavOpen, setIsNavOpen] = useState<boolean>(() => getInitialMobileNavOpen());
  const closeButtonRef = useRef<HTMLButtonElement | null>(null);
  const shellRef = useRef<HTMLDivElement | null>(null);
  const { t } = useTranslation(['workbench']);
  const worktreeStatusLabel = worktree ?? t('workbench:mobile.status.worktree');
  const attentionBadge =
    attentionTotal == null ? null : formatAttentionBadgeCount(attentionTotal);
  const connectionLabel =
    connectionState?.kind === 'online'
      ? t('workbench:mobile.connection.online')
      : connectionState?.kind === 'reconnecting'
        ? t('workbench:mobile.connection.reconnecting')
        : connectionState?.kind === 'offline'
          ? t('workbench:mobile.connection.offline')
          : null;
  const cachedLabel =
    connectionCachedAt != null && connectionState?.kind !== 'online'
      ? t('workbench:mobile.connection.cachedAt', {
          time: new Date(connectionCachedAt).toLocaleString(),
        })
      : null;
  const offlineError =
    connectionState?.kind === 'offline'
      ? t('workbench:mobile.connection.lastError', { error: connectionState.lastError })
      : null;

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

  /**
   * Business Logic（为什么需要这个 effect）:
   *   软键盘弹出时 layout viewport 不变但 visualViewport 变矮，shell 必须压缩并保留顶部菜单可见。
   *
   * Code Logic（这个 effect 做什么）:
   *   监听 visualViewport resize/scroll 与 window resize，写入 --mobile-shell-height 等 CSS 变量。
   */
  useEffect(() => {
    const root = shellRef.current;
    if (!root) return;

    /**
     * Business Logic（为什么需要这个函数）:
     *   viewport 变化后需要同步 shell 高度与终端优先高度 token。
     *
     * Code Logic（这个函数做什么）:
     *   读取 visualViewport/inner*，调用 computeMobileViewportLayoutHints 并写 CSS 变量。
     */
    const applyViewportHints = (): void => {
      const vv = window.visualViewport;
      const offsetTop = vv?.offsetTop ?? 0;
      const hints = computeMobileViewportLayoutHints(
        window.innerWidth,
        window.innerHeight,
        vv?.height ?? null,
        offsetTop,
      );
      // 钉在 visualViewport：键盘弹出时 top/height 同步上移与压缩，内容留在键盘上方。
      root.style.setProperty('--mobile-shell-offset-top', `${Math.max(0, Math.round(offsetTop))}px`);
      root.style.setProperty('--mobile-shell-height', `${hints.shellHeight}px`);
      // inset 仅驱动 data-keyboard-open；CSS 不得再把它叠到 padding-bottom。
      root.style.setProperty('--mobile-keyboard-inset', `${hints.keyboardInset}px`);
      root.style.setProperty('--mobile-terminal-min-height', `${hints.terminalMinHeight}px`);
      root.dataset.landscape = hints.landscape ? 'true' : 'false';
      root.dataset.keyboardOpen = hints.keyboardInset > 0 ? 'true' : 'false';
    };

    applyViewportHints();
    const vv = window.visualViewport;
    vv?.addEventListener('resize', applyViewportHints);
    vv?.addEventListener('scroll', applyViewportHints);
    window.addEventListener('resize', applyViewportHints);
    return () => {
      vv?.removeEventListener('resize', applyViewportHints);
      vv?.removeEventListener('scroll', applyViewportHints);
      window.removeEventListener('resize', applyViewportHints);
    };
  }, []);

  return (
    <div
      ref={shellRef}
      className={styles.shell}
      data-testid="mobile-workbench-shell"
      style={
        {
          // 初始 SSR/首帧回退：真实值由 visualViewport effect 覆盖
          ['--mobile-shell-offset-top' as string]: '0px',
          ['--mobile-shell-height' as string]: '100dvh',
          ['--mobile-keyboard-inset' as string]: '0px',
          ['--mobile-terminal-min-height' as string]: '48dvh',
        } as CSSProperties
      }
    >
      <header className={styles.topbar}>
        <button
          type="button"
          className={styles.menuButton}
          aria-label={t('workbench:mobile.openNavigation')}
          aria-expanded={isNavOpen}
          data-testid="mobile-open-navigation"
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

      <main className={styles.content} data-active-panel={panel}>
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
          {connectionLabel ? (
            <span
              className={styles.statusPill}
              role={connectionState?.kind === 'offline' ? 'status' : undefined}
              data-connection={connectionState?.kind}
            >
              {connectionLabel}
              {cachedLabel ? ` · ${cachedLabel}` : ''}
            </span>
          ) : null}
        </div>
        {offlineError ? (
          <p className={styles.panelState} role="status">
            {offlineError}
          </p>
        ) : null}
        {children}
      </main>
    </div>
  );
}
