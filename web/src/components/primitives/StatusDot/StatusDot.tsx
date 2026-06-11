/**
 * StatusDot 原子组件
 *
 * Business Logic（为什么需要这个组件）:
 *   局域网设备列表、传输任务列表等场景需要在最小空间内传达设备/任务状态，
 *   StatusDot 用一个 8/10px 圆点 + 颜色表达 4 种核心状态，避免重复实现 badge。
 *
 * Code Logic（这个组件做什么）:
 *   渲染一个单色实心圆点；status 决定颜色（online=success / offline=meta / busy=danger / away=warn），
 *   size 决定直径（8/10px）。aria-label 拼接语义化名称以保证可访问性。
 */

import type { HTMLAttributes } from 'react';
import { useTranslation } from 'react-i18next';
import styles from './StatusDot.module.css';

export type StatusDotStatus = 'online' | 'offline' | 'busy' | 'away';
export type StatusDotSize = 'sm' | 'md';

export interface StatusDotProps extends HTMLAttributes<HTMLSpanElement> {
  /** 状态枚举 */
  status: StatusDotStatus;
  /** 尺寸 sm 8px / md 10px */
  size?: StatusDotSize;
  className?: string;
}

/**
 * 渲染统一 StatusDot
 *
 * @param props StatusDot 属性
 * @returns <span> 元素
 */
export function StatusDot(props: StatusDotProps) {
  const { status, size = 'md', className, ...rest } = props;
  const { t } = useTranslation(['common']);
  const label = t(`common:status.device.${status}`);

  const classes = [styles.dot, styles[`status-${status}`], styles[`size-${size}`], className]
    .filter(Boolean)
    .join(' ');

  return (
    <span
      role="status"
      aria-label={label}
      data-status={status}
      data-size={size}
      className={classes}
      {...rest}
    />
  );
}

StatusDot.displayName = 'StatusDot';
