import { describe, test } from 'vitest';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  getWorkbenchBrowserFrameSrc,
  WORKBENCH_BROWSER_IFRAME_SANDBOX,
  canApplyWorkbenchBrowserRequest,
  getWorkbenchBrowserTargetSourceLabelKey,
  isAutoOpenWorkbenchBrowserSource,
  pickAutoOpenWorkbenchBrowserTarget,
} from '@/components/domain/WorkbenchBrowserWorkspace/workbenchBrowserHelpers';
import type {
  WorkbenchBrowserDiscovery,
  WorkbenchBrowserPreview,
  WorkbenchBrowserTarget,
} from '@/lib/types';

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
    assert(
      workspaceViewSource.includes('pickAutoOpenWorkbenchBrowserTarget'),
      'WorkbenchBrowserWorkspace must not auto-open the first discovered target when it is only a port probe',
    );
    assert(
      workspaceViewSource.includes('styles.targetSource')
        && workspaceViewSource.includes('styles.targetUrl'),
      'browser target chips must give the source label and URL separate classes so long URLs cannot stack the source text',
    );

    const workspaceCss = readFileSync(
      new URL(
        '../../components/domain/WorkbenchBrowserWorkspace/WorkbenchBrowserWorkspace.module.css',
        import.meta.url,
      ),
      'utf8',
    );
    assert(
      workspaceCss.includes('.targetSource')
        && workspaceCss.includes('white-space: nowrap')
        && workspaceCss.includes('.targetUrl')
        && workspaceCss.includes('text-overflow: ellipsis'),
      'source label must stay on one line; long display URLs must ellipsis instead of growing the chip height',
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

  test('does not auto-open port probe candidates', () => {
    assert.equal(isAutoOpenWorkbenchBrowserSource('portProbe'), false);
    assert.equal(isAutoOpenWorkbenchBrowserSource('manual'), false);
    assert.equal(isAutoOpenWorkbenchBrowserSource('remembered'), true);
    assert.equal(isAutoOpenWorkbenchBrowserSource('terminalOutput'), true);
    assert.equal(isAutoOpenWorkbenchBrowserSource('projectConfig'), true);

    const probeOnly = discoveryWithTargets(
      [
        probeTarget('http://127.0.0.1:3000/'),
        probeTarget('http://127.0.0.1:5173/'),
        probeTarget('http://127.0.0.1:8080/'),
      ],
      'PortProbe:http://127.0.0.1:3000/',
    );
    assert.equal(pickAutoOpenWorkbenchBrowserTarget(probeOnly), null);

    const noSelection = discoveryWithTargets([probeTarget('http://127.0.0.1:3000/')], null);
    assert.equal(pickAutoOpenWorkbenchBrowserTarget(noSelection), null);

    const terminal = discoveryWithTargets(
      [
        probeTarget('http://127.0.0.1:3000/'),
        {
          ...probeTarget('http://127.0.0.1:5173/'),
          id: 'TerminalOutput:http://127.0.0.1:5173/',
          source: 'terminalOutput',
        },
      ],
      'TerminalOutput:http://127.0.0.1:5173/',
    );
    assert.equal(pickAutoOpenWorkbenchBrowserTarget(terminal)?.url, 'http://127.0.0.1:5173/');
  });
});

function probeTarget(url: string): WorkbenchBrowserTarget {
  return {
    id: `PortProbe:${url}`,
    url,
    displayUrl: url,
    source: 'portProbe',
    label: 'portProbe',
    reachable: true,
  };
}

function discoveryWithTargets(
  targets: WorkbenchBrowserTarget[],
  selectedTargetId: string | null,
): WorkbenchBrowserDiscovery {
  return {
    projectId: 'project-1',
    worktreeId: 'worktree-1',
    targets,
    selectedTargetId,
  };
}
