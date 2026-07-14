import { test as base, expect, type ConsoleMessage, type Page } from '@playwright/test';
import type { TestInfo } from '@playwright/test';
import {
  createBackendHarness,
  type PlaywrightBackendHarness,
} from './support/backendHarness';

/**
 * Business Logic（为什么需要这个类型）:
 *   E2E 诊断需要把浏览器 console.error / pageerror 与用例失败绑定，
 *   以便 CI 与本地复现时直接看到「页面自己抛的错」而不是只看断言失败。
 *
 * Code Logic（这个类型做什么）:
 *   声明 auto fixture `guardBrowserErrors`，对每个用例自动挂载、无需测试体显式引用；
 *   以及 opt-in `backendHarness` 确定性后端注入。
 */
type BrowserDiagnosticsFixtures = {
  guardBrowserErrors: void;
  backendHarness: PlaywrightBackendHarness;
};

/**
 * Business Logic（为什么需要这个函数）:
 *   页面在框选/快照等流程中可能 console.error 或抛未捕获异常，
 *   仅靠 UI 断言会漏掉「界面看似正常但已出错」的回归。
 *
 * Code Logic（这个函数做什么）:
 *   在 page 上注册 console(error 级) 与 pageerror 监听，将消息写入 shared logs 数组。
 *
 * @param page Playwright Page
 * @param logs 可变收集数组，元素为带前缀的错误文本
 */
function installBrowserErrorCollectors(page: Page, logs: string[]): void {
  const onConsole = (msg: ConsoleMessage): void => {
    if (msg.type() === 'error') {
      logs.push(`[console.error] ${msg.text()}`);
    }
  };
  const onPageError = (error: Error): void => {
    logs.push(`[pageerror] ${error.message}`);
  };
  page.on('console', onConsole);
  page.on('pageerror', onPageError);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   失败用例需要把浏览器侧日志作为 Playwright attachment 落盘，
 *   方便在 HTML report / CI artifact 中直接打开，不必重跑抓日志。
 *
 * Code Logic（这个函数做什么）:
 *   若用例失败或收集到浏览器错误，则 attach 名为 `browser-logs` 的 text/plain 内容。
 *
 * @param testInfo 当前用例 TestInfo
 * @param logs 已收集的浏览器错误行
 * @param shouldAttach 是否应写入 attachment
 */
async function attachBrowserLogsIfNeeded(
  testInfo: TestInfo,
  logs: string[],
  shouldAttach: boolean,
): Promise<void> {
  if (!shouldAttach) {
    return;
  }
  const body = logs.length > 0 ? logs.join('\n') : '(no browser console/page errors collected)';
  await testInfo.attach('browser-logs', {
    body,
    contentType: 'text/plain',
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   harness 用例失败时需要把 invoke/fetch/event 调用轨迹附到报告。
 *
 * Code Logic（这个函数做什么）:
 *   attach `harness-calls` JSON。
 */
async function attachHarnessCallLog(
  testInfo: TestInfo,
  harness: PlaywrightBackendHarness,
): Promise<void> {
  await testInfo.attach('harness-calls', {
    body: harness.core.formatCallLog(),
    contentType: 'application/json',
  });
}

/**
 * 带浏览器 console/pageerror 守卫的 Playwright test。
 * 规格文件应从此模块导入 `test` / `expect`，而不是直接从 `@playwright/test`。
 *
 * `backendHarness` 为 opt-in：只有测试参数解构它时才安装；用后 assertSettled，
 * 失败时附加 call log。未使用 harness 的既有 spec 行为不变。
 */
export const test = base.extend<BrowserDiagnosticsFixtures>({
  guardBrowserErrors: [
    async ({ page }, use, testInfo) => {
      /**
       * Business Logic（为什么需要这个 auto fixture）:
       *   所有 E2E 用例都应默认「意外浏览器错误即失败」，并在失败时保留 browser-logs。
       *
       * Code Logic（这个 fixture 做什么）:
       *   挂载监听 → 跑用例 → 失败或有错误时 attach `browser-logs` → 有错误则 expect 失败。
       */
      const logs: string[] = [];
      installBrowserErrorCollectors(page, logs);

      await use();

      const testFailed = testInfo.status !== testInfo.expectedStatus;
      const hasBrowserErrors = logs.length > 0;
      await attachBrowserLogsIfNeeded(testInfo, logs, testFailed || hasBrowserErrors);

      if (hasBrowserErrors) {
        expect(
          logs,
          `Unexpected browser errors:\n${logs.join('\n')}`,
        ).toEqual([]);
      }
    },
    { auto: true },
  ],

  backendHarness: async ({ page }, use, testInfo) => {
    /**
     * Business Logic（为什么需要这个 fixture）:
     *   关键旅程 E2E 需要确定性 Tauri/fetch mock 与结束时 settlement 检查，
     *   但不能强迫既有 ad-hoc mock 用例接入。
     *
     * Code Logic（这个 fixture 做什么）:
     *   创建 harness → install(page) → 交给用例 → assertSettled；
     *   失败或 settlement 异常时 attach harness-calls。
     */
    const harness = createBackendHarness();
    await harness.install(page);

    // Playwright fixture 第二参固定名 `use`（非 React Hook）。
    // eslint-disable-next-line react-hooks/rules-of-hooks -- Playwright fixture `use` callback
    await use(harness);

    const testFailed = testInfo.status !== testInfo.expectedStatus;
    try {
      harness.assertSettled();
    } catch (error) {
      await attachHarnessCallLog(testInfo, harness);
      throw error;
    }

    if (testFailed) {
      await attachHarnessCallLog(testInfo, harness);
    }
  },
});

export { expect };
