/**
 * Sidebar（侧边导航栏）
 *
 * Business Logic（为什么需要这个组件）:
 *   主窗口需要一个固定的左侧导航区域，集中展示 Logo / 导航项 /
 *   用户/版本信息等 footer；短窗口下内容区独立滚动，footer 不被覆盖。
 *
 * Code Logic（这个组件做什么）:
 *   渲染一个 240px 宽、高度填满父级的 flex column：content 使用
 *   min-height:0 + overflow-y:auto 独立滚动；footer 留在 flex 流底部。
 */
import type { ReactNode } from 'react';
import styles from './Sidebar.module.css';

export interface SidebarProps {
  /** 顶部主内容区（Logo + NavItem 列表等） */
  children: ReactNode;
  /** 底部 footer 插槽（版本号、用户信息等），固定在 flex 流底部 */
  footer?: ReactNode;
  /** 透传的自定义 className */
  className?: string;
}

export function Sidebar({ children, footer, className }: SidebarProps) {
  const cls = [styles.sidebar, className].filter(Boolean).join(' ');
  return (
    <aside className={cls}>
      <div className={styles.content}>{children}</div>
      {footer ? <div className={styles.footer}>{footer}</div> : null}
    </aside>
  );
}
