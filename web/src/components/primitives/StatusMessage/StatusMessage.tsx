/**
 * StatusMessage 异步反馈原语
 *
 * Business Logic（为什么需要这个组件）:
 *   保存、Git mutation、转存/删除等异步结果需要统一的 live region 反馈：
 *   成功用 role=status 礼貌播报，阻断失败用 role=alert 恰好一次，避免各页自建 toast/alert 合同不一致。
 *
 * Code Logic（这个组件做什么）:
 *   按 tone 映射 role/aria-live 与视觉样式；支持 action 插槽（如重试）与 className 透传；
 *   无业务语义、无 API 依赖，仅渲染反馈容器。
 */

import type { HTMLAttributes, ReactNode } from 'react';
import styles from './StatusMessage.module.css';

/** 反馈语义色调 */
export type StatusMessageTone = 'success' | 'info' | 'warn' | 'danger';

/** live region 策略；默认由 tone 推导 */
export type StatusMessageLive = 'polite' | 'assertive' | 'off';

export interface StatusMessageProps extends Omit<HTMLAttributes<HTMLDivElement>, 'children'> {
  /** 反馈正文 */
  children: ReactNode;
  /** 语义色调：danger → alert，其余 → status */
  tone?: StatusMessageTone;
  /** 覆盖默认 live region（danger=assertive，其余=polite） */
  live?: StatusMessageLive;
  /** 可选动作区（重试按钮等） */
  action?: ReactNode;
}

/**
 * 根据 tone 推导默认 live region
 *
 * Business Logic（为什么需要这个函数）:
 *   阻断失败需 assertive，成功/信息保持 polite，避免轮询类提示抢占读屏。
 *
 * Code Logic（这个函数做什么）:
 *   danger → assertive；其它 tone → polite。
 */
function defaultLiveForTone(tone: StatusMessageTone): Exclude<StatusMessageLive, 'off'> {
  return tone === 'danger' ? 'assertive' : 'polite';
}

/**
 * 根据 tone 推导 ARIA role
 *
 * Business Logic（为什么需要这个函数）:
 *   读屏需要区分「状态更新」与「需要立即关注的错误」。
 *
 * Code Logic（这个函数做什么）:
 *   danger → alert；其余 → status。
 */
function roleForTone(tone: StatusMessageTone): 'status' | 'alert' {
  return tone === 'danger' ? 'alert' : 'status';
}

/**
 * 渲染统一异步反馈消息
 *
 * Business Logic（为什么需要这个函数）:
 *   页面与 domain 组件应复用同一反馈表面，而不是各自拼 role/aria-live。
 *
 * Code Logic（这个函数做什么）:
 *   输出带 data-tone 的容器，设置 role/aria-live，可选渲染 action。
 */
export function StatusMessage(props: StatusMessageProps) {
  const {
    children,
    tone = 'info',
    live,
    action,
    className,
    ...rest
  } = props;

  const resolvedLive = live ?? defaultLiveForTone(tone);
  const role = roleForTone(tone);
  const classes = [styles.message, styles[`tone-${tone}`], className].filter(Boolean).join(' ');

  return (
    <div
      role={role}
      aria-live={resolvedLive}
      data-tone={tone}
      className={classes}
      {...rest}
    >
      <div className={styles.body}>{children}</div>
      {action ? <div className={styles.action}>{action}</div> : null}
    </div>
  );
}

StatusMessage.displayName = 'StatusMessage';
