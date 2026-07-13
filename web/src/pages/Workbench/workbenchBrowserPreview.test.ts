import { describe, test } from 'vitest';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  getWorkbenchBrowserFrameSrc,
  WORKBENCH_BROWSER_IFRAME_SANDBOX,
  canApplyWorkbenchBrowserRequest,
  getWorkbenchBrowserTargetSourceLabelKey,
} from '@/components/domain/WorkbenchBrowserWorkspace/workbenchBrowserHelpers';
import type { WorkbenchBrowserPreview } from '@/lib/types';

describe('workbenchBrowserPreview', () => {
  test('chooses frame src, sandbox tokens, request gate and source label keys', () => {
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

    const iframeSandboxTokens = new Set(WORKBENCH_BROWSER_IFRAME_SANDBOX.split(/\s+/));
    assert.equal(
      iframeSandboxTokens.has('allow-scripts'),
      true,
      'preview iframe should allow project scripts for dev server previews',
    );
    assert.equal(
      iframeSandboxTokens.has('allow-same-origin'),
      false,
      'preview iframe must omit allow-same-origin to block preview content from same-origin cc-partner APIs such as /api/mobile and /api/workbench',
    );
    const workspaceViewSource = readFileSync(
      new URL(
        '../../components/domain/WorkbenchBrowserWorkspace/WorkbenchBrowserWorkspaceView.tsx',
        import.meta.url,
      ),
      'utf8',
    );
    assert(
      workspaceViewSource.includes('sandbox={WORKBENCH_BROWSER_IFRAME_SANDBOX}'),
      'WorkbenchBrowserWorkspace iframe should apply the shared sandbox policy',
    );
    assert(
      !workspaceViewSource.includes('allow-same-origin'),
      'WorkbenchBrowserWorkspace view must not opt preview iframe back into cc-partner same-origin access',
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
  });
});
