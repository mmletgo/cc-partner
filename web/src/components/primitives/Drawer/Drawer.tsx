/**
 * Drawer 原子组件
 *
 * Business Logic（为什么需要这个组件）:
 *   窄屏导航、任务详情侧栏等需要侧滑 modal 面板；与 Dialog 共享层栈合同，
 *   避免业务页复制 focus trap / inert / Escape 逻辑。
 *
 * Code Logic（这个组件做什么）:
 *   portal 到 document.body；surface 使用 role=dialog + aria-modal + aria-labelledby；
 *   surface 默认提供内容内边距（padding: var(--space-5)），直接塞内容不再贴边；
 *   header/body 自管分区 padding 或全宽分隔线时，调用方 className 须 padding:0 覆盖；
 *   side 控制左/右侧滑；useModalLayer 管理焦点与层栈；默认 closeOnEscape/closeOnBackdrop=true。
 */

import { useCallback, useRef, type MouseEvent, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { useModalLayer } from '../Dialog/useModalLayer';
import type { DialogProps } from '../Dialog/Dialog';
import styles from './Drawer.module.css';

export interface DrawerProps extends DialogProps {
  side?: 'left' | 'right';
}

/**
 * 渲染可访问侧滑 Drawer
 *
 * Business Logic（为什么需要这个函数）:
 *   调用方以 Dialog 同构 API 打开侧栏，仅额外声明 side。
 *
 * Code Logic（这个函数做什么）:
 *   hooks 全在 early return 前；open 时 portal；surface 标记 data-side 与 ARIA。
 */
export function Drawer(props: DrawerProps): ReactNode {
  const {
    open,
    titleId,
    children,
    initialFocusRef,
    closeOnEscape = true,
    closeOnBackdrop = true,
    onClose,
    className,
    side = 'right',
  } = props;

  const surfaceRef = useRef<HTMLDivElement | null>(null);

  useModalLayer({
    open,
    surfaceRef,
    initialFocusRef,
    closeOnEscape,
    onClose,
  });

  /**
   * backdrop 点击按策略关闭
   *
   * Business Logic（为什么需要这个函数）:
   *   用户点击遮罩关闭抽屉（可配置）。
   *
   * Code Logic（这个函数做什么）:
   *   closeOnBackdrop 为 true 时调用 onClose。
   */
  const handleBackdropClick = useCallback(() => {
    if (closeOnBackdrop) {
      onClose();
    }
  }, [closeOnBackdrop, onClose]);

  /**
   * 阻止 surface 点击冒泡
   *
   * Business Logic（为什么需要这个函数）:
   *   点击抽屉内容不得触发 backdrop 关闭。
   *
   * Code Logic（这个函数做什么）:
   *   stopPropagation。
   */
  const handleSurfaceClick = useCallback((event: MouseEvent<HTMLDivElement>) => {
    event.stopPropagation();
  }, []);

  if (!open) {
    return null;
  }

  if (typeof document === 'undefined') {
    return null;
  }

  const surfaceClass = [
    styles.surface,
    side === 'left' ? styles.sideLeft : styles.sideRight,
    className,
  ]
    .filter(Boolean)
    .join(' ');

  return createPortal(
    <div className={styles.root} data-drawer-root data-side={side}>
      <div
        className={styles.backdrop}
        data-drawer-backdrop
        onClick={handleBackdropClick}
        aria-hidden="true"
      />
      <div
        ref={surfaceRef}
        className={surfaceClass}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        data-side={side}
        tabIndex={-1}
        onClick={handleSurfaceClick}
      >
        {children}
      </div>
    </div>,
    document.body,
  );
}
