import { defineConfig, mergeConfig } from 'vitest/config';
import viteConfig from './vite.config';

export default mergeConfig(
  viteConfig,
  defineConfig({
    test: {
      environment: 'node',
      globals: false,
      // support/ 下的纯 harness 合同可走 Vitest；E2E *.spec.ts 仍只由 Playwright 收集
      include: ['src/**/*.test.{ts,tsx}', 'tests/support/**/*.test.ts'],
      exclude: ['tests/**/*.spec.ts', 'node_modules/**', 'dist/**'],
      passWithNoTests: false,
    },
  }),
);
