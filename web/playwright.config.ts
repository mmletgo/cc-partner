import { defineConfig, devices } from '@playwright/test';

/**
 * Playwright E2E 配置。
 *
 * Business Logic（为什么需要这份配置）:
 *   前端 L1 旅程统一在 Chromium 上跑；mobile 场景用 per-test viewport（见
 *   `tests/support/backendHarness.ts` 的 MOBILE_VIEWPORT），不新增第二套 browser project。
 *
 * Code Logic（这份配置做什么）:
 *   单 project `chromium` + Vite webServer；失败保留 screenshot/trace/video。
 */
export default defineConfig({
  testDir: './tests',
  // support/*.test.ts 由 Vitest 收集；此处忽略以免 Playwright 误跑
  testIgnore: ['**/support/**'],
  outputDir: 'test-results',
  timeout: 30_000,
  retries: process.env.CI ? 1 : 0,
  expect: {
    timeout: 2_000,
  },
  webServer: {
    command: 'npm run dev -- --host 127.0.0.1',
    url: 'http://127.0.0.1:5173',
    reuseExistingServer: !process.env.CI,
  },
  use: {
    baseURL: 'http://127.0.0.1:5173',
    screenshot: 'only-on-failure',
    trace: 'retain-on-failure',
    video: 'retain-on-failure',
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
});
