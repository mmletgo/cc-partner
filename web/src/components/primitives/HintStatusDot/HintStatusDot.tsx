/**
 * 工作台等待/完成数字标点。
 *
 * Business Logic（为什么需要这个组件）:
 *   项目卡、worktree、窗口 tab 要在现有状态点上叠数字；无 hint 时必须回到原点语义。
 *
 * Code Logic（这个组件做什么）:
 *   count>0 时放大写 formatAttentionBadgeCount，并打 data-hint-tone；否则只渲染原点。
 */

import type { HTMLAttributes } from 'react';
import { formatAttentionBadgeCount } from '@/lib/attention';
import type { AgentHintTone } from '@/lib/workbenchAgentHints';
import styles from './HintStatusDot.module.css';

export interface HintStatusDotProps extends HTMLAttributes<HTMLSpanElement> {
  count: number;
  tone: AgentHintTone | null;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   三处标点必须共用同一套放大/数字/颜色规则，避免 rail/tab 各自漂移。
 *
 * Code Logic（这个组件做什么）:
 *   组合 className；有数字时 aria 不 hidden。
 */
export function HintStatusDot({
  count,
  tone,
  className,
  ...rest
}: HintStatusDotProps) {
  const label = formatAttentionBadgeCount(count);
  const hinted = Boolean(label && tone);
  const classes = [className, hinted ? styles.hinted : null].filter(Boolean).join(' ');
  return (
    <span
      className={classes}
      data-hint-tone={hinted ? tone : undefined}
      aria-hidden={hinted ? undefined : true}
      {...rest}
    >
      {hinted ? label : null}
    </span>
  );
}
