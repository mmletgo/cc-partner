/**
 * 工作台等待/已停止数字标点。
 *
 * Business Logic（为什么需要这个组件）:
 *   项目卡、worktree、窗口 tab 必须始终能看到等待/已停止数量，0 也要写出来。
 *
 * Code Logic（这个组件做什么）:
 *   永远写 waiting/stopped，并打 data-hint-tone=wait|complete|zero。
 */

import type { HTMLAttributes } from 'react';
import { formatHintCount, type AgentHintTone } from '@/lib/workbenchAgentHints';
import styles from './HintStatusDot.module.css';

export interface HintStatusDotProps extends HTMLAttributes<HTMLSpanElement> {
  waitingCount: number;
  stoppedCount: number;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   三处标点必须共用同一套放大/数字/颜色规则，避免 rail/tab 各自漂移。
 *
 * Code Logic（这个组件做什么）:
 *   永远写 waiting/stopped；等待优先着色，双 0 用 zero tone。
 */
export function HintStatusDot({
  waitingCount,
  stoppedCount,
  className,
  ...rest
}: HintStatusDotProps) {
  const waiting = formatHintCount(waitingCount);
  const stopped = formatHintCount(stoppedCount);
  const tone: AgentHintTone =
    waitingCount > 0 ? 'wait' : stoppedCount > 0 ? 'complete' : 'zero';
  const classes = [className, styles.hinted].filter(Boolean).join(' ');
  return (
    <span className={classes} data-hint-tone={tone} {...rest}>
      {waiting}/{stopped}
    </span>
  );
}
