import assert from 'node:assert/strict';
import { getWorkbenchBrowserFrameSrc } from '@/components/domain/WorkbenchBrowserWorkspace/WorkbenchBrowserWorkspace';
import type { WorkbenchBrowserPreview } from '@/lib/types';

const preview: WorkbenchBrowserPreview = {
  previewId: 'preview-1',
  projectId: 'project-1',
  worktreeId: 'worktree-1',
  targetUrl: 'http://127.0.0.1:5173/',
  desktopProxyUrl: 'http://127.0.0.1:62116/api/workbench/browser/proxy/preview-1/',
  mobileProxyPath: '/api/mobile/workbench/browser/proxy/preview-1/',
  expiresAtMs: 1893456000000,
};

assert.equal(
  getWorkbenchBrowserFrameSrc(preview, 'desktop'),
  'http://127.0.0.1:62116/api/workbench/browser/proxy/preview-1/',
);
assert.equal(
  getWorkbenchBrowserFrameSrc(preview, 'mobile'),
  '/api/mobile/workbench/browser/proxy/preview-1/',
);

console.log('workbenchBrowserPreview tests passed');
