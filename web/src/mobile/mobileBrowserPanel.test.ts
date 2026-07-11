import { describe, test } from 'vitest';
import { getWorkbenchBrowserFrameSrc } from '@/components/domain/WorkbenchBrowserWorkspace/WorkbenchBrowserWorkspace';
import type { WorkbenchBrowserPreview } from '@/lib/types';

/**
 * Business Logic（为什么需要这个函数）:
 *   当前 web tsconfig 会编译 src 下测试文件，但未启用 Node 类型；测试断言需要避免依赖 node:assert。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛出 Error，让用例失败。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

const preview: WorkbenchBrowserPreview = {
  previewId: 'mobile-preview',
  projectId: 'project-1',
  worktreeId: null,
  targetUrl: 'http://127.0.0.1:5173/',
  desktopProxyUrl: 'http://127.0.0.1:62116/api/workbench/browser/proxy/mobile-preview/',
  mobileProxyPath: '/api/mobile/workbench/browser/proxy/mobile-preview/',
  expiresAtMs: 1893456000000,
};

describe('mobileBrowserPanel', () => {
  test('uses the mobile proxy path as the browser frame src', () => {
    assert(
      getWorkbenchBrowserFrameSrc(preview, 'mobile') ===
        '/api/mobile/workbench/browser/proxy/mobile-preview/',
      'mobile browser frame src should be the mobile proxy path',
    );
    assert(
      getWorkbenchBrowserFrameSrc(preview, 'mobile') !== preview.desktopProxyUrl,
      'mobile browser frame src should differ from desktop proxy url',
    );
  });
});
