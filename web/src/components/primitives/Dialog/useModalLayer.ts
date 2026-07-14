/**
 * useModalLayer — 共享模态层栈 hook
 *
 * Business Logic（为什么需要这个 hook）:
 *   Dialog/Drawer 需要一致的可访问性合同（焦点陷阱、Escape、背景 inert、滚动锁、关闭后焦点恢复），
 *   且嵌套模态时只有最顶层响应键盘；把层栈逻辑集中在一处可避免业务页各自实现陷阱与泄漏。
 *
 * Code Logic（这个 hook 做什么）:
 *   维护模块级 openLayers 栈；open 时锁定 body 滚动、按顶层同步背景 inert/aria-hidden、
 *   聚焦 initialFocusRef 或首个可聚焦元素（否则 surface tabIndex=-1）；仅顶层处理 Escape/Tab；
 *   close/unmount 时恢复属性、滚动与触发元素焦点。
 */

import { useEffect, useRef, type RefObject } from 'react';

export interface ModalLayerOptions {
  open: boolean;
  surfaceRef: RefObject<HTMLElement | null>;
  initialFocusRef?: RefObject<HTMLElement | null>;
  closeOnEscape: boolean;
  onClose: () => void;
}

/** 模块级：当前打开的 surface 栈（末尾为顶层） */
const openLayers: HTMLElement[] = [];

/** 当前被层栈强制 inert 的 body 子节点 */
const managedInert = new Set<Element>();

/** 元素被托管前的 aria-hidden 原值（null 表示原先没有该属性） */
const previousAriaHidden = new WeakMap<Element, string | null>();

/** 元素被托管前是否已有 inert=true */
const previousInert = new WeakMap<Element, boolean>();

/** body overflow 引用计数与打开前原值 */
let scrollLockCount = 0;
let previousBodyOverflow = '';

const FOCUSABLE_SELECTOR = [
  'a[href]',
  'button:not([disabled])',
  'input:not([disabled]):not([type="hidden"])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',');

/**
 * 判断元素是否可见且可参与焦点循环
 *
 * Business Logic（为什么需要这个函数）:
 *   隐藏或禁用控件不应参与 Tab 循环，否则键盘用户会落到不可见目标。
 *
 * Code Logic（这个函数做什么）:
 *   过滤 disabled/hidden/aria-hidden/不可见 display:none|visibility:hidden 的元素。
 */
function isFocusableCandidate(el: HTMLElement): boolean {
  if (el.hasAttribute('disabled') || el.getAttribute('aria-disabled') === 'true') {
    return false;
  }
  if (el.hidden || el.getAttribute('aria-hidden') === 'true') {
    return false;
  }
  const style = window.getComputedStyle(el);
  if (style.display === 'none' || style.visibility === 'hidden') {
    return false;
  }
  return true;
}

/**
 * 收集 surface 内可聚焦元素（已过滤 disabled/hidden）
 *
 * Business Logic（为什么需要这个函数）:
 *   Tab 循环只应在真实可交互控件间移动。
 *
 * Code Logic（这个函数做什么）:
 *   querySelectorAll 标准可聚焦选择器后用 isFocusableCandidate 过滤。
 */
export function getFocusableElements(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    isFocusableCandidate,
  );
}

/**
 * 定位挂到 document.body 的直接子节点（portal root）
 *
 * Business Logic（为什么需要这个函数）:
 *   inert 标记应对 body 兄弟节点生效，surface 可能嵌在 portal 包装层内。
 *
 * Code Logic（这个函数做什么）:
 *   从 surface 向上走到 parentElement === document.body 的节点；找不到则返回自身。
 */
function getPortalRoot(surface: HTMLElement): HTMLElement {
  let current: HTMLElement = surface;
  while (current.parentElement && current.parentElement !== document.body) {
    current = current.parentElement;
  }
  return current;
}

/**
 * 强制元素 inert + aria-hidden，并保存旧值
 *
 * Business Logic（为什么需要这个函数）:
 *   背景层必须对辅助技术与 Tab 不可达。
 *
 * Code Logic（这个函数做什么）:
 *   首次托管时保存旧属性，写入 aria-hidden="true" 与 inert。
 */
function forceInert(el: Element): void {
  previousAriaHidden.set(el, el.getAttribute('aria-hidden'));
  const htmlEl = el as HTMLElement & { inert?: boolean };
  previousInert.set(el, Boolean(htmlEl.inert));
  el.setAttribute('aria-hidden', 'true');
  if ('inert' in htmlEl) {
    htmlEl.inert = true;
  } else {
    el.setAttribute('inert', '');
  }
}

/**
 * 恢复元素被托管前的 inert/aria-hidden
 *
 * Business Logic（为什么需要这个函数）:
 *   层关闭或不再作为背景后，页面应回到打开前的可访问状态。
 *
 * Code Logic（这个函数做什么）:
 *   还原 previousAriaHidden / previousInert 并清理托管记录。
 */
function restoreInert(el: Element): void {
  const prev = previousAriaHidden.get(el);
  previousAriaHidden.delete(el);
  if (prev === null || prev === undefined) {
    el.removeAttribute('aria-hidden');
  } else {
    el.setAttribute('aria-hidden', prev);
  }
  const htmlEl = el as HTMLElement & { inert?: boolean };
  const wasInert = previousInert.get(el) ?? false;
  previousInert.delete(el);
  if ('inert' in htmlEl) {
    htmlEl.inert = wasInert;
  }
  if (!wasInert) {
    el.removeAttribute('inert');
  }
}

/**
 * 按当前 openLayers 顶层同步 body 子节点 inert
 *
 * Business Logic（为什么需要这个函数）:
 *   嵌套模态时仅最顶层 portal 可交互，下层与页面内容必须 inert；
 *   不能用“各自 acquire 除自己外全部节点”的方式，否则后开层会被先开层永久 inert。
 *
 * Code Logic（这个函数做什么）:
 *   顶层 portal root 保持可交互；其余 body 子节点强制 inert；对差分集合 force/restore。
 */
function syncBackgroundInert(): void {
  const topSurface = openLayers[openLayers.length - 1];
  const topRoot = topSurface ? getPortalRoot(topSurface) : null;

  const shouldManage = new Set<Element>();
  if (topRoot) {
    for (const child of Array.from(document.body.children)) {
      if (child !== topRoot) {
        shouldManage.add(child);
      }
    }
  }

  for (const el of Array.from(managedInert)) {
    if (!shouldManage.has(el)) {
      restoreInert(el);
      managedInert.delete(el);
    }
  }

  for (const el of shouldManage) {
    if (!managedInert.has(el)) {
      forceInert(el);
      managedInert.add(el);
    }
  }
}

/**
 * 引用计数锁定 body 滚动
 *
 * Business Logic（为什么需要这个函数）:
 *   模态打开时页面不应滚动；嵌套时不可提前解锁。
 *
 * Code Logic（这个函数做什么）:
 *   首次锁定保存 overflow 并设 hidden；归零时恢复。
 */
function lockBodyScroll(): void {
  if (scrollLockCount === 0) {
    previousBodyOverflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
  }
  scrollLockCount += 1;
}

/**
 * 引用计数解锁 body 滚动
 *
 * Business Logic（为什么需要这个函数）:
 *   所有模态关闭后恢复页面滚动。
 *
 * Code Logic（这个函数做什么）:
 *   计数减一，归零恢复 previousBodyOverflow。
 */
function unlockBodyScroll(): void {
  if (scrollLockCount === 0) return;
  scrollLockCount -= 1;
  if (scrollLockCount === 0) {
    document.body.style.overflow = previousBodyOverflow;
    previousBodyOverflow = '';
  }
}

/**
 * 判断 surface 是否为当前顶层模态
 *
 * Business Logic（为什么需要这个函数）:
 *   只有顶层应处理 Escape 与 Tab，避免下层抢键。
 *
 * Code Logic（这个函数做什么）:
 *   比较 openLayers 末尾是否等于 surface。
 */
function isTopLayer(surface: HTMLElement): boolean {
  return openLayers[openLayers.length - 1] === surface;
}

/**
 * 在 surface 内放置初始焦点
 *
 * Business Logic（为什么需要这个函数）:
 *   打开后键盘应进入模态内容，避免焦点仍留在触发器或页面上。
 *
 * Code Logic（这个函数做什么）:
 *   优先 initialFocusRef；否则首个可聚焦子元素；否则 focus surface（需 tabIndex=-1）。
 */
function focusInitial(
  surface: HTMLElement,
  initialFocusRef?: RefObject<HTMLElement | null>,
): void {
  const preferred = initialFocusRef?.current;
  if (preferred && surface.contains(preferred)) {
    preferred.focus();
    return;
  }
  const focusables = getFocusableElements(surface);
  if (focusables.length > 0) {
    focusables[0].focus();
    return;
  }
  if (surface.tabIndex < 0) {
    surface.tabIndex = -1;
  }
  surface.focus();
}

/**
 * 共享模态层副作用 hook
 *
 * Business Logic（为什么需要这个函数）:
 *   Dialog 与 Drawer 共用同一层栈合同，保证嵌套安全与键盘可达。
 *
 * Code Logic（这个函数做什么）:
 *   open 时入栈、锁滚动、同步背景 inert、设焦点、绑定 keydown；close/unmount 清理并恢复触发焦点。
 */
export function useModalLayer(options: ModalLayerOptions): void {
  const { open, surfaceRef, initialFocusRef, closeOnEscape, onClose } = options;

  // 用 ref 保存最新回调/配置，避免 effect 因 onClose 引用变化反复拆装层
  const onCloseRef = useRef(onClose);
  const closeOnEscapeRef = useRef(closeOnEscape);
  const initialFocusRefRef = useRef(initialFocusRef);

  // 保存打开时的触发元素，关闭后恢复焦点
  const triggerRef = useRef<HTMLElement | null>(null);
  // 记录本 effect 实例是否已成功注册层，避免重复 cleanup
  const registeredRef = useRef(false);

  // 在 effect 中同步最新 props，避免 render 阶段写 ref（react-hooks/refs）
  useEffect(() => {
    onCloseRef.current = onClose;
    closeOnEscapeRef.current = closeOnEscape;
    initialFocusRefRef.current = initialFocusRef;
  }, [onClose, closeOnEscape, initialFocusRef]);

  useEffect(() => {
    if (!open) {
      return undefined;
    }

    const surface = surfaceRef.current;
    if (!surface) {
      return undefined;
    }

    const active = document.activeElement;
    triggerRef.current =
      active instanceof HTMLElement && active !== document.body ? active : null;

    openLayers.push(surface);
    registeredRef.current = true;
    lockBodyScroll();
    syncBackgroundInert();

    // 等 portal/children 提交后再聚焦
    const focusFrame = window.requestAnimationFrame(() => {
      focusInitial(surface, initialFocusRefRef.current);
    });

    /**
     * 顶层键盘处理：Escape 关闭；Tab 在 surface 内循环
     */
    const handleKeyDown = (event: KeyboardEvent): void => {
      if (!isTopLayer(surface)) {
        return;
      }

      if (event.key === 'Escape') {
        if (!closeOnEscapeRef.current) {
          return;
        }
        event.preventDefault();
        event.stopPropagation();
        onCloseRef.current();
        return;
      }

      if (event.key !== 'Tab') {
        return;
      }

      const focusables = getFocusableElements(surface);
      if (focusables.length === 0) {
        event.preventDefault();
        surface.focus();
        return;
      }

      const first = focusables[0];
      const last = focusables[focusables.length - 1];
      const current = document.activeElement as HTMLElement | null;

      if (event.shiftKey) {
        if (current === first || !surface.contains(current)) {
          event.preventDefault();
          last.focus();
        }
        return;
      }

      if (current === last || !surface.contains(current)) {
        event.preventDefault();
        first.focus();
      }
    };

    document.addEventListener('keydown', handleKeyDown, true);

    return () => {
      window.cancelAnimationFrame(focusFrame);
      document.removeEventListener('keydown', handleKeyDown, true);

      if (registeredRef.current) {
        const index = openLayers.lastIndexOf(surface);
        if (index >= 0) {
          openLayers.splice(index, 1);
        }
        syncBackgroundInert();
        unlockBodyScroll();
        registeredRef.current = false;
      }

      const trigger = triggerRef.current;
      triggerRef.current = null;
      // 仅在当前无更高层时恢复焦点，避免嵌套关闭时抢焦点
      if (trigger && openLayers.length === 0) {
        if (document.contains(trigger)) {
          trigger.focus();
        }
      } else if (trigger && openLayers.length > 0) {
        // 关闭顶层后焦点落到新的顶层 surface
        const nextTop = openLayers[openLayers.length - 1];
        if (nextTop && document.contains(nextTop)) {
          window.requestAnimationFrame(() => {
            if (openLayers[openLayers.length - 1] === nextTop) {
              focusInitial(nextTop);
            }
          });
        }
      }
    };
  }, [open, surfaceRef]);
}
