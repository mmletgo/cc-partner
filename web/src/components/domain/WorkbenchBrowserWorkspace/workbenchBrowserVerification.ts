/**
 * workbenchBrowserVerification — 浏览器验证 pure helpers
 *
 * Business Logic（为什么需要这个模块）:
 *   面板与测试需要稳定构造默认 smoke 请求、判断终态、拼截图 data URL。
 *
 * Code Logic（这个模块做什么）:
 *   无副作用工具函数。
 */

import type { BrowserVerificationRun, BrowserVerificationState } from '@/lib/types';

/**
 * Business Logic（为什么需要这个函数）:
 *   一键验证默认不要求用户写脚本或选 selector。
 *
 * Code Logic（这个函数做什么）:
 *   返回仅含 previewId 与 requestId 的启动参数对象。
 */
export function buildDefaultVerificationStart(previewId: string, requestId: string): {
  previewId: string;
  requestId: string;
} {
  return { previewId, requestId };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   UI 轮询需要知道 run 是否已结束。
 *
 * Code Logic（这个函数做什么）:
 *   判断 state 是否为 succeeded/failed/canceled。
 */
export function isBrowserVerificationTerminal(state: BrowserVerificationState): boolean {
  return state === 'succeeded' || state === 'failed' || state === 'canceled';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   结果区需要简短状态文案 key。
 *
 * Code Logic（这个函数做什么）:
 *   映射 state → i18n key 后缀。
 */
export function verificationStatusKey(state: BrowserVerificationState): string {
  return `browserVerification.status.${state}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   截图 base64 需要变成 img src。
 *
 * Code Logic（这个函数做什么）:
 *   拼 data:image/png;base64,...；空则 null。
 */
export function screenshotDataUrl(base64: string | null | undefined): string | null {
  if (!base64) return null;
  return `data:image/png;base64,${base64}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   结果摘要需要 console 错误数与 assertion 失败数。
 *
 * Code Logic（这个函数做什么）:
 *   从 run.evidence 提取计数。
 */
export function summarizeVerification(run: BrowserVerificationRun | null): {
  consoleErrors: number;
  assertionFailed: number;
  urlPath: string | null;
  screenshotId: string | null;
} {
  if (!run?.evidence) {
    return { consoleErrors: 0, assertionFailed: 0, urlPath: null, screenshotId: null };
  }
  const assertionFailed = run.evidence.assertions.filter((a) => !a.passed).length;
  return {
    consoleErrors: run.evidence.consoleErrors.length,
    assertionFailed,
    urlPath: run.evidence.urlPath,
    screenshotId: run.evidence.screenshotId ?? null,
  };
}
