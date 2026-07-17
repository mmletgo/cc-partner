/**
 * Dialog 原子组件
 *
 * Business Logic（为什么需要这个组件）:
 *   应用内多处需要模态确认/表单（创建任务、危险操作、设置子弹），
 *   统一 portal + focus trap + Escape/backdrop 合同，避免业务页各自实现键盘陷阱与焦点泄漏。
 *
 * Code Logic（这个组件做什么）:
 *   打开时 portal 到 document.body，渲染 backdrop + role=dialog surface；
 *   surface 默认提供内容内边距（padding: var(--space-5)），直接塞标题/表单不再贴边；
 *   嵌套 Card 或 header/body 自管 padding 时，调用方 className 须 padding:0 覆盖；
 *   通过 useModalLayer 管理层栈、焦点陷阱、inert、滚动锁与触发焦点恢复；
 *   无业务导入；closeOnEscape/closeOnBackdrop 默认 true。
 */

import {
  useCallback,
  useRef,
  type MouseEvent,
  type ReactNode,
  type RefObject,
} from 'react';
import { createPortal } from 'react-dom';
import { useModalLayer } from './useModalLayer';
import styles from './Dialog.module.css';

export interface DialogProps {
  open: boolean;
  titleId: string;
  children: ReactNode;
  initialFocusRef?: RefObject<HTMLElement | null>;
  closeOnEscape?: boolean;
  closeOnBackdrop?: boolean;
  onClose: () => void;
  className?: string;
}

/**
 * 渲染可访问 Dialog 模态
 *
 * Business Logic（为什么需要这个函数）:
 *   调用方只需提供 open/titleId/onClose 与内容，即可获得一致的模态交互合同。
 *
 * Code Logic（这个函数做什么）:
 *   hooks 全在 early return 前；open 时 createPortal 挂到 body；surface 带 ARIA 与 tabIndex=-1。
 */
export function Dialog(props: DialogProps): ReactNode {
  const {
    open,
    titleId,
    children,
    initialFocusRef,
    closeOnEscape = true,
    closeOnBackdrop = true,
    onClose,
    className,
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
   * backdrop 点击：按策略关闭；忽略非主按钮
   *
   * Business Logic（为什么需要这个函数）:
   *   用户点击遮罩期望关闭弹层（可配置关闭）。
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
   * 阻止 surface 内点击冒泡到 root（若未来 root 也绑 click）
   *
   * Business Logic（为什么需要这个函数）:
   *   点击对话框内容不得被当成 backdrop 关闭。
   *
   * Code Logic（这个函数做什么）:
   *   stopPropagation 于 surface mousedown/click。
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

  const surfaceClass = [styles.surface, className].filter(Boolean).join(' ');

  return createPortal(
    <div className={styles.root} data-dialog-root>
      <div
        className={styles.backdrop}
        data-dialog-backdrop
        onClick={handleBackdropClick}
        aria-hidden="true"
      />
      <div
        ref={surfaceRef}
        className={surfaceClass}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        onClick={handleSurfaceClick}
      >
        {children}
      </div>
    </div>,
    document.body,
  );
}
