import { useRef } from 'react';
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
 *   click 重复触发 onPrimary。onPrimary 为统一动作入口；ref 与 inline handler 都封装在子组件内，
 *   父组件不接触 ref，符合 react-hooks/refs 规则。
 */
export interface PointerPrimaryButtonProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'onClick' | 'onPointerDown'> {
  /** 主动作；pointerDown（触摸/鼠标）与键盘 click 兜底都调用它。 */
  onPrimary: (event: SyntheticEvent) => void;
  /** pointerDown 阶段、preventDefault 之后、disabled 判断之前执行（如收起软键盘）；即使 disabled 也调用。 */
  beforePointerDown?: (event: ReactPointerEvent<HTMLButtonElement>) => void;
}

export function PointerPrimaryButton({
  onPrimary,
  beforePointerDown,
  disabled,
  children,
  ...rest
}: PointerPrimaryButtonProps): ReactElement {
  const handledByPointerRef = useRef(false);
  return (
    <button
      {...rest}
      disabled={disabled}
      onPointerDown={(event) => {
        // 阻止 button 获焦，避免焦点离开终端/工具栏。
        event.preventDefault();
        beforePointerDown?.(event);
        if (disabled) return;
        handledByPointerRef.current = true;
        onPrimary(event);
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
