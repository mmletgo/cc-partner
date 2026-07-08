import assert from 'node:assert/strict';
import {
  getWorkbenchBrowserFrameSrc,
  canApplyWorkbenchBrowserRequest,
  getWorkbenchBrowserTargetSourceLabelKey,
} from '@/components/domain/WorkbenchBrowserWorkspace/WorkbenchBrowserWorkspace';
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

assert.equal(
  canApplyWorkbenchBrowserRequest(
    { sequence: 2, projectId: 'project-2', worktreeId: 'worktree-2' },
    { sequence: 1, projectId: 'project-1', worktreeId: 'worktree-1' },
  ),
  false,
  'stale discovery from an old project/worktree must not select a preview',
);

assert.equal(
  canApplyWorkbenchBrowserRequest(
    { sequence: 2, projectId: 'project-1', worktreeId: 'worktree-1' },
    { sequence: 1, projectId: 'project-1', worktreeId: 'worktree-1' },
  ),
  false,
  'stale openTarget click in the same context must not replace the latest preview',
);

assert.equal(
  canApplyWorkbenchBrowserRequest(
    { sequence: 2, projectId: 'project-1', worktreeId: null },
    { sequence: 2, projectId: 'project-1', worktreeId: null },
  ),
  true,
  'latest request for the current context may apply its preview',
);

assert.equal(
  getWorkbenchBrowserTargetSourceLabelKey('remembered'),
  'workbench:browserPreview.sources.remembered',
  'remembered source should render through workbench i18n instead of backend label text',
);
assert.equal(
  getWorkbenchBrowserTargetSourceLabelKey('terminalOutput'),
  'workbench:browserPreview.sources.terminalOutput',
  'terminal source should render through workbench i18n instead of backend label text',
);

console.log('workbenchBrowserPreview tests passed');
