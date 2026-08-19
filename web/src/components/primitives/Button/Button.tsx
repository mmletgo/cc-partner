/**
 * Button 原子组件
 *
 * Business Logic（为什么需要这个组件）:
 *   整个应用有大量需要触发动作的交互点（保存/删除/复制/确认等），
 *   统一按钮视觉与行为可保证品牌一致性，并让表单/Dialog/Toolbar 等上层组件不必重复处理
 *   loading/disabled/icon 占位等细节。
 *
 * Code Logic（这个组件做什么）:
 *   基于原生 <button> 渲染的受控展示组件，支持 5 种 variant、3 种 size、loading/icon 状态；
 *   使用 React.forwardRef 暴露底层 button 引用以支持父级聚焦/工具库绑定（如 tippy/floating-ui）；
 *   loading=true 时禁用点击，spinner 叠在原内容上，不改按钮宽高，也不改 accessible name。
 */

import { forwardRef } from 'react';
import type { ButtonHTMLAttributes, ReactNode } from 'react';
import styles from './Button.module.css';

export type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'danger' | 'icon';
export type ButtonSize = 'sm' | 'md' | 'lg';

export interface ButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'children'> {
  /** 视觉变体，primary 为强调操作，icon 为纯图标方形按钮 */
  variant?: ButtonVariant;
  /** 尺寸，sm/md/lg 分别对应 26/32/40px 高度 */
  size?: ButtonSize;
  /** 禁用态 */
  disabled?: boolean;
  /** 加载中：禁用点击，原地叠 spinner，尺寸与文案与 idle 一致 */
  loading?: boolean;
  /** 左侧图标 */
  icon?: ReactNode;
  /** 右侧图标 */
  iconRight?: ReactNode;
  /** 按钮文本 */
  children?: ReactNode;
  /** 透传 className，用于布局（margin 等） */
  className?: string;
}

/**
 * 渲染应用统一按钮
 *
 * @param props 继承原生 button 属性 + 视觉/状态扩展
 * @returns 标准 <button> 元素，ref 已 forward
 */
export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(props, ref) {
  const {
    variant = 'secondary',
    size = 'md',
    disabled = false,
    loading = false,
    icon,
    iconRight,
    children,
    type = 'button',
    className,
    onClick,
    ...rest
  } = props;

  // icon 变体忽略 size；其他变体拼接 class
  const classes = [
    styles.button,
    variant === 'icon' ? styles['variant-icon'] : styles[`variant-${variant}`],
    variant === 'icon' ? '' : styles[`size-${size}`],
    className,
  ]
    .filter(Boolean)
    .join(' ');

  /**
   * 内部点击处理：loading 时短路阻止点击冒泡
   *
   * @param e React 鼠标事件
   */
  const handleClick = (e: React.MouseEvent<HTMLButtonElement>) => {
    if (loading || disabled) {
      e.preventDefault();
      return;
    }
    onClick?.(e);
  };

  return (
    <button
      ref={ref}
      type={type}
      data-variant={variant}
      data-size={size}
      data-loading={loading || undefined}
      disabled={disabled || loading}
      className={classes}
      onClick={handleClick}
      {...rest}
    >
      {loading ? <span className={styles.spinner} aria-hidden="true" /> : null}
      <span className={styles.inner}>
        {icon ? <span className={styles.iconWrap}>{icon}</span> : null}
        {variant !== 'icon' ? children : null}
        {iconRight ? <span className={styles.iconWrap}>{iconRight}</span> : null}
      </span>
    </button>
  );
});

Button.displayName = 'Button';
