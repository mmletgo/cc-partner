import { useCallback, useEffect, useRef, useState } from 'react';
import type { ComponentType, CSSProperties, ReactElement, ReactNode, SVGProps } from 'react';
import { useTranslation } from 'react-i18next';
import { Drawer } from '@/components/primitives';
import { formatAttentionBadgeCount } from '@/lib/attention';
import {
  BellIcon,
  BrowserIcon,
  ChevronLeftIcon,
  FileIcon,
  FolderIcon,
  ForkIcon,
  HistoryIcon,
  MenuIcon,
  OrchestratorIcon,
  ProviderManagerIcon,
  SendIcon,
  SettingsIcon,
  TerminalIcon,
  XIcon,
} from '@/lib/icons';
import {
  closeMobileNav,
  computeMobileKeyboardShift,
  computeMobileViewportLayoutHints,
  getInitialMobileNavOpen,
  getMobileWorkbenchNavGroups,
  isMobileEditableKeyboardTarget,
  isMobileTerminalTypingTarget,
  resolveAppliedMobileKeyboardShift,
  openMobileNav,
  resolveMobileNavMode,
  selectMobilePanel,
  type MobileConnectionState,
  type MobileWorkbenchNavGroupId,
  type MobileWorkbenchNavMode,
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
  transfer: SendIcon,
  automation: OrchestratorIcon,
  settings: SettingsIcon,
  provider: ProviderManagerIcon,
};

const MOBILE_NAV_GROUP_TITLE_IDS: Record<MobileWorkbenchNavGroupId, string> = {
  projects: 'mobile-nav-group-projects',
  inbox: 'mobile-nav-group-inbox',
  tools: 'mobile-nav-group-tools',
  system: 'mobile-nav-group-system',
  work: 'mobile-nav-group-work',
  shortcuts: 'mobile-nav-group-shortcuts',
};

export interface MobileWorkbenchShellProps {
  panel: MobileWorkbenchPanel;
  project: string | null;
  worktree: string | null;
  session: string | null;
  /** 是否已选中可进入工作台的项目；驱动 global/project 导航模式。 */
  hasActiveProject?: boolean;
  onPanelChange: (panel: MobileWorkbenchPanel) => void;
  /** 项目工作台导航中的「返回项目列表」；未传则不渲染返回按钮。 */
  onBackToProjects?: () => void;
  /** Attention 总数；0/null 不显示 badge，规则与桌面 formatAttentionBadgeCount 一致。 */
  attentionTotal?: number | null;
  /** 弱网连接态；offline/reconnecting 时展示缓存时间。 */
  connectionState?: MobileConnectionState | null;
  /** 可展示的缓存起点 epoch ms。 */
  connectionCachedAt?: number | null;
  /**
   * 工作区 worktree 条（对齐桌面 worktreeBar）。由父级按面板决定是否传入；
   * 渲染在状态栏上方、面板滚动区之外，避免终端焦点把条滚出视口。
   */
  worktreeStrip?: ReactNode;
  children: ReactNode;
}

interface MobilePanelNavProps {
  activePanel: MobileWorkbenchPanel;
  navMode: MobileWorkbenchNavMode;
  onSelect: (panel: MobileWorkbenchPanel) => void;
  onBackToProjects?: () => void;
  attentionBadge: string | null;
  projectLabel: string | null;
}

/**
 * MobilePanelNav（移动端工作台分组导航）
 *
 * Business Logic（为什么需要这个组件）:
 *   全局壳只展示项目/待处理/传输/设置；进入项目后切换为项目内工具 + 全局快捷，
 *   与桌面「先选项目再进工作台」一致。
 *
 * Code Logic（这个组件做什么）:
 *   按 navMode 取 getMobileWorkbenchNavGroups 渲染 section + panel；project 模式顶部提供返回项目。
 */
function MobilePanelNav({
  activePanel,
  navMode,
  onSelect,
  onBackToProjects,
  attentionBadge,
  projectLabel,
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
    transfer: t('workbench:mobile.nav.transfer'),
    automation: t('workbench:mobile.nav.automation'),
    settings: t('workbench:mobile.nav.settings'),
    provider: t('workbench:mobile.nav.provider'),
  };
  const groupLabels: Record<MobileWorkbenchNavGroupId, string> = {
    projects: t('workbench:mobile.navGroups.projects'),
    inbox: t('workbench:mobile.navGroups.inbox'),
    tools: t('workbench:mobile.navGroups.tools'),
    system: t('workbench:mobile.navGroups.system'),
    work: t('workbench:mobile.navGroups.work'),
    shortcuts: t('workbench:mobile.navGroups.shortcuts'),
  };

  return (
    <nav
      className={styles.navList}
      aria-label={t('workbench:mobile.navAriaLabel')}
      data-nav-mode={navMode}
    >
      {navMode === 'project' && onBackToProjects ? (
        <button
          type="button"
          className={styles.navBackButton}
          data-testid="mobile-nav-back-to-projects"
          onClick={onBackToProjects}
        >
          <ChevronLeftIcon size={16} aria-hidden="true" />
          <span className={styles.navBackText}>
            <span className={styles.navBackLabel}>
              {t('workbench:mobile.nav.backToProjects')}
            </span>
            {projectLabel ? (
              <span className={styles.navBackProject}>{projectLabel}</span>
            ) : null}
          </span>
        </button>
      ) : null}
      {getMobileWorkbenchNavGroups(navMode).map((group) => {
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
 *   工作区面板由父级注入固定 worktree 条（不随面板滚动）；状态行只读；软键盘弹出时按终端/焦点计算上移量。
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
  hasActiveProject = false,
  onPanelChange,
  onBackToProjects,
  attentionTotal = null,
  connectionState = null,
  connectionCachedAt = null,
  worktreeStrip = null,
  children,
}: MobileWorkbenchShellProps): ReactElement {
  const [isNavOpen, setIsNavOpen] = useState<boolean>(() => getInitialMobileNavOpen());
  const closeButtonRef = useRef<HTMLButtonElement | null>(null);
  const shellRef = useRef<HTMLDivElement | null>(null);
  const { t } = useTranslation(['workbench']);
  const navMode = resolveMobileNavMode(panel, hasActiveProject);
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
   * Business Logic（为什么需要这个函数）:
   *   项目工作台内需要一键回到全局项目列表，与桌面离开当前项目上下文对齐。
   *
   * Code Logic（这个函数做什么）:
   *   调用 onBackToProjects（通常切到 projects 面板）并关闭抽屉。
   */
  const handleBackToProjects = useCallback((): void => {
    if (!onBackToProjects) return;
    onBackToProjects();
    setIsNavOpen(closeMobileNav());
  }, [onBackToProjects]);

  /**
   * Business Logic（为什么需要这个 effect）:
   *   软键盘弹出时 layout viewport 通常不变但 visualViewport 变矮；终端输入要把 shell /
   *   全屏 overlay 整体顶到键盘上方，其它输入只把焦点抬到未遮挡可视区中线附近。
   *
   * Code Logic（这个 effect 做什么）:
   *   监听 visualViewport resize/scroll、window resize 与 focusin/focusout；写入
   *   --mobile-shell-height / --mobile-keyboard-inset / --mobile-keyboard-shift /
   *   --mobile-terminal-min-height 与 data-keyboard-open；弹层 portal 同步 transform。
   */
  useEffect(() => {
    const root = shellRef.current;
    if (!root) return;
    const shiftRef = { current: 0 };

    /**
     * Business Logic（为什么需要这个函数）:
     *   Dialog portal 在 document.body，不继承 shell 的 top 上移，必须同步平移否则焦点仍被键盘盖住。
     *
     * Code Logic（这个函数做什么）:
     *   给 [data-dialog-root] 写入/清除 translateY(-shift)；shift=0 时去掉 transform，避免多余 containing block。
     */
    const applyPortalShift = (shift: number): void => {
      const nodes = document.querySelectorAll<HTMLElement>('[data-dialog-root]');
      nodes.forEach((node) => {
        if (shift > 0) {
          node.style.transform = `translateY(-${shift}px)`;
        } else {
          node.style.removeProperty('transform');
        }
      });
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   viewport 或焦点变化后需要同步 shell 高度、键盘占用、实际上移量与终端优先高度 token。
     *
     * Code Logic（这个函数做什么）:
     *   读取 visualViewport/inner* 与 activeElement，计算 inset 与 shift，写 CSS 变量并平移弹层。
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
      const active = document.activeElement;
      const terminalTyping = isMobileTerminalTypingTarget(active);
      const editable = isMobileEditableKeyboardTarget(active);
      let focusTop: number | null = null;
      let focusHeight = 0;
      if (editable && active instanceof HTMLElement) {
        const rect = active.getBoundingClientRect();
        const dialogRoot = active.closest('[data-dialog-root]');
        const appliedShift = resolveAppliedMobileKeyboardShift({
          dialogTransform:
            dialogRoot instanceof HTMLElement ? dialogRoot.style.transform || null : null,
          insideShell: root.contains(active),
          shellShift: shiftRef.current,
        });
        focusTop = rect.top + appliedShift;
        focusHeight = rect.height;
      }
      const shift = computeMobileKeyboardShift({
        keyboardInset: hints.keyboardInset,
        layoutViewportHeight: hints.shellHeight,
        focusTop,
        focusHeight,
        mode: terminalTyping ? 'full' : 'focused',
        previousShift: shiftRef.current,
      });
      shiftRef.current = shift;
      root.style.setProperty('--mobile-shell-height', `${hints.shellHeight}px`);
      // inset = 键盘占用高度；shift = 实际 CSS 上移量。CSS 不得把 inset 再叠到 padding-bottom。
      root.style.setProperty('--mobile-keyboard-inset', `${hints.keyboardInset}px`);
      root.style.setProperty('--mobile-keyboard-shift', `${shift}px`);
      document.documentElement.style.setProperty('--mobile-keyboard-shift', `${shift}px`);
      root.style.setProperty('--mobile-terminal-min-height', `${hints.terminalMinHeight}px`);
      root.dataset.landscape = hints.landscape ? 'true' : 'false';
      root.dataset.keyboardOpen = hints.keyboardInset > 0 ? 'true' : 'false';
      applyPortalShift(shift);
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   focusout 时下一个可编辑焦点可能尚未挂上，立即按「无焦点」计算会让页面先掉下去再抬起。
     *
     * Code Logic（这个函数做什么）:
     *   下一帧再跑 applyViewportHints，让同一次切焦点的 focusin 先写入。
     */
    const handleFocusOut = (): void => {
      window.requestAnimationFrame(applyViewportHints);
    };

    applyViewportHints();
    const vv = window.visualViewport;
    vv?.addEventListener('resize', applyViewportHints);
    vv?.addEventListener('scroll', applyViewportHints);
    window.addEventListener('resize', applyViewportHints);
    document.addEventListener('focusin', applyViewportHints);
    document.addEventListener('focusout', handleFocusOut);
    return () => {
      vv?.removeEventListener('resize', applyViewportHints);
      vv?.removeEventListener('scroll', applyViewportHints);
      window.removeEventListener('resize', applyViewportHints);
      document.removeEventListener('focusin', applyViewportHints);
      document.removeEventListener('focusout', handleFocusOut);
      document.documentElement.style.removeProperty('--mobile-keyboard-shift');
      applyPortalShift(0);
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
          ['--mobile-shell-height' as string]: '100dvh',
          ['--mobile-keyboard-inset' as string]: '0px',
          ['--mobile-keyboard-shift' as string]: '0px',
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
          <h2 id={MOBILE_NAV_DRAWER_TITLE_ID} className={styles.srOnlyTitle}>
            {t('workbench:mobile.openNavigation')}
          </h2>
          <p className={styles.topMeta}>{project ?? t('workbench:mobile.projectFallback')}</p>
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
          navMode={navMode}
          onSelect={handleSelectPanel}
          onBackToProjects={onBackToProjects ? handleBackToProjects : undefined}
          attentionBadge={attentionBadge}
          projectLabel={project}
        />
      </Drawer>

      <aside className={styles.rail} aria-label={t('workbench:mobile.railAriaLabel')}>
        <div className={styles.railHeader}>
          <p className={styles.topMeta}>{project ?? t('workbench:mobile.noProject')}</p>
        </div>
        <MobilePanelNav
          activePanel={panel}
          navMode={navMode}
          onSelect={handleSelectPanel}
          onBackToProjects={onBackToProjects ? handleBackToProjects : undefined}
          attentionBadge={attentionBadge}
          projectLabel={project}
        />
      </aside>

      <main className={styles.content} data-active-panel={panel}>
        {worktreeStrip ? (
          <div className={styles.mobileWorktreeChrome} data-testid="mobile-worktree-chrome">
            {worktreeStrip}
          </div>
        ) : null}
        <div className={styles.statusRow} aria-label={t('workbench:mobile.statusAriaLabel')}>
          <span className={styles.statusPill}>{project ?? t('workbench:mobile.status.project')}</span>
          <span className={styles.statusPill}>{worktreeStatusLabel}</span>
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
        <div className={styles.contentBody}>{children}</div>
      </main>
    </div>
  );
}
