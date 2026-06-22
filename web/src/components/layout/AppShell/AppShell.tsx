/**
 * AppShell（整个应用外壳）
 *
 * Business Logic（为什么需要这个组件）:
 *   Claude Partner 是一个三端（macOS / Windows / Linux）桌面工具，
 *   Web 端需要提供侧边导航 + 主内容区的基本布局骨架，
 *   窗口标题栏由 PyQt6 原生提供，无需 Web 端自绘。
 *   侧边栏 footer 区域集中展示版本号和主题切换。
 *
 * Code Logic（这个组件做什么）:
 *   - 全屏 flex 布局：左侧 Sidebar（240px）+ 右侧 main 区域
 *   - Sidebar 内包含 Logo、导航项、footer（版本号 + ThemeToggle）
 *   - 右侧 main 区域是 <Outlet /> 出口，由 React Router 注入子页面，
 *     main 自带 overflow: auto 实现独立滚动
 *
 *   注意：本组件是 <Outlet /> 容器，children 不直接使用。
 */
import { Outlet } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import {
  HomeIcon,
  TransferIcon,
  PromptsIcon,
  HistoryIcon,
  ScratchpadIcon,
  DevicesIcon,
  SettingsIcon,
} from '../../../lib/icons';
import { useAppVersion } from '../../../hooks/useAppVersion';
import { Sidebar } from '../Sidebar';
import { NavItem } from '../NavItem';
import { ThemeToggle } from '../ThemeToggle';
import { LanguageSwitcher } from '../LanguageSwitcher';
import { PermissionStatusBadge } from '@/components/domain';
import styles from './AppShell.module.css';

export interface AppShellProps {
  /** 路由出口占位（一般由 react-router 注入 <Outlet />，可显式覆盖） */
  children?: React.ReactNode;
}

export function AppShell({ children }: AppShellProps) {
  // 版本号以后端 __init__.py 的 __version__ 为唯一权威来源，通过 useAppVersion
  // 从 /api/version 动态获取，前端不再硬编码，避免发版漏改导致版本不一致。
  const version = useAppVersion();
  // 传入命名空间数组,让 react-i18next v17 的 t() 类型校验 ns:key 形式
  // (无参时 t() 只接受 defaultNS 即 common 的扁平 key,'nav:*' 会类型报错)
  const { t } = useTranslation(['common', 'nav']);
  return (
    <div className={styles.layout}>
      <Sidebar
        footer={
          <div className={styles.footer}>
            <span className={styles.footerVersion}>v{version ?? '—'}</span>
            <span>Claude Partner</span>
            <div className={styles.footerToggle}>
              <LanguageSwitcher />
              <ThemeToggle />
            </div>
          </div>
        }
      >
        <div className={styles.logo}>
          <span className={styles.logoMark} aria-hidden="true">
            CP
          </span>
          <span className={styles.logoText}>Claude Partner</span>
        </div>
        <nav className={styles.navList} aria-label="primary">
          <NavItem to="/" label={t('nav:home')} icon={<HomeIcon />} />
          <NavItem to="/transfer" label={t('nav:transfer')} icon={<TransferIcon />} />
          <NavItem to="/prompts" label={t('nav:prompts')} icon={<PromptsIcon />} />
          <NavItem to="/cc-history" label={t('nav:ccHistory')} icon={<HistoryIcon />} />
          <NavItem to="/scratchpad" label={t('nav:scratchpad')} icon={<ScratchpadIcon />} />
          <NavItem to="/devices" label={t('nav:devices')} icon={<DevicesIcon />} />
          <NavItem to="/settings" label={t('nav:settings')} icon={<SettingsIcon />} />
        </nav>
        <PermissionStatusBadge />
      </Sidebar>
      <main className={styles.main}>{children ?? <Outlet />}</main>
    </div>
  );
}
