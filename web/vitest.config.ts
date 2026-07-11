import { defineConfig, mergeConfig } from 'vitest/config';
import viteConfig from './vite.config';
import { legacyTestPaths } from './scripts/test-migration-manifest.mjs';

export default mergeConfig(
  viteConfig,
  defineConfig({
    test: {
      environment: 'node',
      globals: false,
      include: ['src/**/*.test.{ts,tsx}'],
      exclude: ['tests/**', 'node_modules/**', 'dist/**', ...legacyTestPaths],
      passWithNoTests: false,
    },
  }),
);
