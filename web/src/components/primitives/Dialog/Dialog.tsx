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
 *   backdrop 关闭只接受「在遮罩上起手」的完整主指针手势，避免打开触发器的同一次 tap
 *   （pointerdown 打开 → 合成 click 落到刚挂载的遮罩）把弹层立刻关掉；
 *   无业务导入；closeOnEscape/closeOnBackdrop 默认 true；
 *   backdropVariant 默认 frost（半透明+blur），scrim 仅半透明无模糊。
 */

import {
  useCallback,
  useRef,
  type MouseEvent,
  type PointerEvent as ReactPointerEvent,
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
  /**
   * frost（默认）：半透明 + 毛玻璃；
   * scrim：仅半透明遮罩，不加 blur（Game Hub 等需要看清背景时用）。
   */
  backdropVariant?: 'frost' | 'scrim';
}

/**
 * 渲染可访问 Dialog 模态
 *
 * Business Logic（为什么需要这个函数）:
 *   调用方只需提供 open/titleId/onClose 与内容，即可获得一致的模态交互合同。
 *
 * Code Logic（这个函数做什么）:
 *   hooks 全在 early return 前；open 时 createPortal 挂到 body；surface 带 ARIA 与 tabIndex=-1。
 *   backdrop 关闭只接受「在遮罩上起手」的完整主指针手势；
 *   backdropVariant=scrim 时去掉 blur，只保留半透明遮罩。
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
    backdropVariant = 'frost',
  } = props;

  const surfaceRef = useRef<HTMLDivElement | null>(null);
  /** 当前主指针是否在 backdrop 上按下；click 关闭必须匹配，防止 ghost click。 */
  const backdropPointerDownRef = useRef(false);

  useModalLayer({
    open,
    surfaceRef,
    initialFocusRef,
    closeOnEscape,
    onClose,
  });

  /**
   * 记录 backdrop 主指针按下，供后续 click 判定是否为本手势。
   *
   * Business Logic（为什么需要这个函数）:
   *   移动端用 pointerdown 打开 sheet 时，同一次手势的合成 click 常落在刚挂载的遮罩上；
   *   若只监听 click，会把「打开」误判为「点遮罩关闭」，弹层一闪即灭。
   *
   * Code Logic（这个函数做什么）:
   *   仅主按钮（button===0，含触摸）且目标为 backdrop 自身时置位；忽略冒泡自 surface 的事件。
   */
  const handleBackdropPointerDown = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      if (event.button !== 0) {
        backdropPointerDownRef.current = false;
        return;
      }
      backdropPointerDownRef.current = event.target === event.currentTarget;
    },
    [],
  );

  /**
   * backdrop 点击：仅当 closeOnBackdrop 且本手势在遮罩上起手时关闭。
   *
   * Business Logic（为什么需要这个函数）:
   *   用户点击遮罩期望关闭弹层（可配置关闭），但不能吃掉打开按钮的同一次 tap。
   *
   * Code Logic（这个函数做什么）:
   *   要求 pointerdown 已在 backdrop 上记录，且 click 目标仍是 backdrop 自身；消费后清标记。
   */
  const handleBackdropClick = useCallback(
    (event: MouseEvent<HTMLDivElement>) => {
      const startedOnBackdrop = backdropPointerDownRef.current;
      backdropPointerDownRef.current = false;
      if (!closeOnBackdrop) return;
      if (!startedOnBackdrop) return;
      if (event.target !== event.currentTarget) return;
      onClose();
    },
    [closeOnBackdrop, onClose],
  );

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
        className={[styles.backdrop, backdropVariant === 'scrim' ? styles.backdropScrim : '']
          .filter(Boolean)
          .join(' ')}
        data-dialog-backdrop
        data-backdrop-variant={backdropVariant}
        onPointerDown={handleBackdropPointerDown}
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
