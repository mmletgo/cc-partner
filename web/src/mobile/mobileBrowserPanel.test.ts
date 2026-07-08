import assert from 'node:assert/strict';
import { getWorkbenchBrowserFrameSrc } from '@/components/domain/WorkbenchBrowserWorkspace/WorkbenchBrowserWorkspace';
import type { WorkbenchBrowserPreview } from '@/lib/types';

const preview: WorkbenchBrowserPreview = {
  previewId: 'mobile-preview',
  projectId: 'project-1',
  worktreeId: null,
  targetUrl: 'http://127.0.0.1:5173/',
  desktopProxyUrl: 'http://127.0.0.1:62116/api/workbench/browser/proxy/mobile-preview/',
  mobileProxyPath: '/api/mobile/workbench/browser/proxy/mobile-preview/',
  expiresAtMs: 1893456000000,
};

assert.equal(
  getWorkbenchBrowserFrameSrc(preview, 'mobile'),
  '/api/mobile/workbench/browser/proxy/mobile-preview/',
);
assert.notEqual(getWorkbenchBrowserFrameSrc(preview, 'mobile'), preview.desktopProxyUrl);

console.log('mobileBrowserPanel tests passed');
