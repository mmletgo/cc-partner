/**
 * 移动端终端从后台回到前台时，把视口钉到最新输出。
 *
 * Business Logic（为什么需要这个模块）:
 *   手机浏览器缩到后台后 JS / rAF / NDJSON 会暂停；回来时 xterm 仍停在离开时的 viewport，
 *   用户必须手动滚过积压输出。恢复可见时应显示最新内容。
 *
 * Code Logic（这个模块做什么）:
 *   纯判定与绝对 scrollToLine(baseY)；pin 窗口覆盖断线重连 catch-up，用户手势滚动则取消。
 */

/** 恢复可见后继续跟随 live catch-up 的时长（覆盖立刻重连 + Gap 首批写入）。 */
export const MOBILE_TERMINAL_RESUME_FOLLOW_MS = 8_000;

export interface MobileTerminalResumePin {
  untilMs: number;
}

export interface MobileTerminalResumeFollowTarget {
  buffer: { active: { baseY: number; viewportY: number } };
  scrollToLine: (line: number) => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   hidden 时不应把视口拉到底；只有页面真正回到前台才跟最新输出。
 *
 * Code Logic（这个函数做什么）:
 *   visibilityState 严格等于 visible 时返回 true。
 */
export function isMobileTerminalResumeVisible(
  visibilityState: DocumentVisibilityState | string,
): boolean {
  return visibilityState === 'visible';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   划选复制时不能抢滚动，否则选区会丢；其余前台恢复都应跟到底。
 *
 * Code Logic（这个函数做什么）:
 *   可见且未在划选、当前无文本选区时才允许 follow。
 */
export function shouldFollowMobileTerminalToLatest(input: {
  visible: boolean;
  selecting: boolean;
  hasSelection: boolean;
}): boolean {
  return input.visible && !input.selecting && !input.hasSelection;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   回到前台瞬间 buffer 可能还是旧的；事件流重连后还会追加一段 catch-up，必须短暂钉在底部。
 *
 * Code Logic（这个函数做什么）:
 *   以 nowMs + duration 生成 pin；非法 duration 按 0 处理。
 */
export function beginMobileTerminalResumePin(
  nowMs: number,
  durationMs: number = MOBILE_TERMINAL_RESUME_FOLLOW_MS,
): MobileTerminalResumePin {
  const safeDuration = Number.isFinite(durationMs) && durationMs > 0 ? durationMs : 0;
  return { untilMs: nowMs + safeDuration };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   live 增量到达时只有仍在 pin 窗口内才自动跟到底，避免用户已经开始回看历史时被拽回。
 *
 * Code Logic（这个函数做什么）:
 *   pin 非空且 nowMs 严格小于 untilMs。
 */
export function isMobileTerminalResumePinned(
  pin: MobileTerminalResumePin | null,
  nowMs: number,
): boolean {
  return pin !== null && nowMs < pin.untilMs;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手机浏览器上 xterm 相对 scrollLines / scrollToBottom 可能与 buffer 游标失步，必须用绝对行号。
 *
 * Code Logic（这个函数做什么）:
 *   把 viewport 设到 baseY（最新一行所在屏）。
 */
export function scrollMobileTerminalToLatest(
  terminal: MobileTerminalResumeFollowTarget,
): void {
  terminal.scrollToLine(Math.max(0, terminal.buffer.active.baseY));
}
