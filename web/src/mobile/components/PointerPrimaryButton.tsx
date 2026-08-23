import { useEffect, useRef } from 'react';
import type {
  ButtonHTMLAttributes,
  PointerEvent as ReactPointerEvent,
  ReactElement,
  SyntheticEvent,
} from 'react';

/**
 * 移动端「按下即触发」按钮。
 *
 * Business Logic（为什么需要这个组件）:
 *   移动端浏览器在软键盘/输入法激活时，对非编辑区的首次 tap 常被系统用于收起键盘或取消 IME 组合，
 *   不产生 click 事件；把按钮主逻辑放在 onClick 会导致「输入法激活时首次点击无效」（要点第二次）。
 *
 * Code Logic（这个组件做什么）:
 *   以 pointerDown 为主触发路径（在系统键盘/IME 收起逻辑之前可靠触发），click 作为键盘无障碍兜底
 *   （Enter/Space，无 pointer 前导时触发）；用 ref 记录 pointer 已处理，避免 pointerDown 与随后的
 *   click 重复触发 onPrimary。可选 repeat：按下立刻触发一次，按住超过 delay 后再按 interval 连发，
 *   仅 pointer 按住路径生效。onPrimary 为统一动作入口；ref 与 inline handler 都封装在子组件内，
 *   父组件不接触 ref，符合 react-hooks/refs 规则。
 */
export interface PointerPrimaryButtonRepeat {
  /** 按下立刻触发一次后，再按住这么久才开始连发。 */
  delayMs: number;
  /** 开始连发后的间隔。 */
  intervalMs: number;
}

export interface PointerPrimaryButtonProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'onClick' | 'onPointerDown'> {
  /** 主动作；pointerDown（触摸/鼠标）与键盘 click 兜底都调用它。 */
  onPrimary: (event: SyntheticEvent) => void;
  /** pointerDown 阶段、preventDefault 之后、disabled 判断之前执行（如收起软键盘）；即使 disabled 也调用。 */
  beforePointerDown?: (event: ReactPointerEvent<HTMLButtonElement>) => void;
  /**
   * 按住连发。仅 pointer 按住路径生效：按下立刻触发一次，delayMs 后再按 intervalMs 连发；
   * 松手 / cancel / 卸载停止。键盘 click 兜底不连发。
   */
  repeat?: PointerPrimaryButtonRepeat;
}

export function PointerPrimaryButton({
  onPrimary,
  beforePointerDown,
  disabled,
  children,
  repeat,
  ...rest
}: PointerPrimaryButtonProps): ReactElement {
  const handledByPointerRef = useRef(false);
  const delayTimerRef = useRef<number | null>(null);
  const intervalTimerRef = useRef<number | null>(null);

  /**
   * Business Logic（为什么需要这个函数）:
   *   松手、取消、卸载或再次按下都必须清掉连发定时器，否则会在抬手后继续向终端灌键。
   *
   * Code Logic（这个函数做什么）:
   *   清 delay timeout 与 interval，并把 id 置空。
   */
  function stopRepeat(): void {
    if (delayTimerRef.current != null) {
      window.clearTimeout(delayTimerRef.current);
      delayTimerRef.current = null;
    }
    if (intervalTimerRef.current != null) {
      window.clearInterval(intervalTimerRef.current);
      intervalTimerRef.current = null;
    }
  }

  useEffect(() => {
    return () => {
      stopRepeat();
    };
  }, []);

  useEffect(() => {
    if (disabled) stopRepeat();
  }, [disabled]);

  return (
    <button
      {...rest}
      disabled={disabled}
      onPointerDown={(event) => {
        // 阻止 button 获焦，避免焦点离开终端/工具栏。
        event.preventDefault();
        beforePointerDown?.(event);
        stopRepeat();
        if (disabled) return;
        handledByPointerRef.current = true;
        if (repeat) {
          try {
            event.currentTarget.setPointerCapture(event.pointerId);
          } catch {
            // jsdom 或宿主不支持 capture 时仍继续按住手势。
          }
        }
        onPrimary(event);
        if (!repeat) return;
        const fireHeld = () => onPrimary(event);
        delayTimerRef.current = window.setTimeout(() => {
          delayTimerRef.current = null;
          fireHeld();
          intervalTimerRef.current = window.setInterval(fireHeld, repeat.intervalMs);
        }, repeat.delayMs);
      }}
      onPointerUp={() => {
        stopRepeat();
      }}
      onPointerCancel={() => {
        stopRepeat();
      }}
      onClick={(event) => {
        // pointerDown 已处理 pointer 路径，跳过避免双触发；这里仅兜底键盘 Enter/Space（无 pointer 前导）。
        if (handledByPointerRef.current) {
          handledByPointerRef.current = false;
          return;
        }
        if (disabled) return;
        onPrimary(event);
      }}
    >
      {children}
    </button>
  );
}
