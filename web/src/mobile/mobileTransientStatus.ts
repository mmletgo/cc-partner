/**
 * 移动端短暂成功提示。
 *
 * Business Logic（为什么需要这个模块）:
 *   提交成功、复制成功、Prompt 写入、自动化创建等确认只应短暂出现，不能一直钉在面板上。
 *
 * Code Logic（这个模块做什么）:
 *   导出自动消失时长与 hook：value 非空时定时把 setter 写成 null。
 */

import { useEffect } from 'react';

/** 成功提示展示时长（与桌面 merge stage 自动收起对齐）。 */
export const MOBILE_TRANSIENT_STATUS_MS = 2500;

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端成功确认是短暂反馈；组件高频重渲染（终端输出）不能把 timer 重置掉。
 *
 * Code Logic（这个函数做什么）:
 *   value 非空时 setTimeout，到期调用 setValue(null)；value 变化或卸载时清旧 timer。
 *   setValue 必须是 useState setter（引用稳定）。
 */
export function useAutoDismissedStatus(
  value: string | null,
  setValue: (next: string | null) => void,
  delayMs: number = MOBILE_TRANSIENT_STATUS_MS,
): void {
  useEffect(() => {
    if (!value) return undefined;
    const timer = window.setTimeout(() => {
      setValue(null);
    }, delayMs);
    return () => window.clearTimeout(timer);
  }, [value, setValue, delayMs]);
}
