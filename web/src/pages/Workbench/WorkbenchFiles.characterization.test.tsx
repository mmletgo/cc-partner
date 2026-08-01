// @vitest-environment jsdom
/**
 * Workbench 文件域 characterization 测试。
 *
 * Business Logic: 锁住目录展开/选中、打开文件创建 tab、保存与格式化的 stale 响应丢弃、dirty tab 关闭
 * 确认流程等当前可观察行为。
 *
 * Code Logic: 通过真实文件树 DOM 点击驱动 onSelect/onToggle；通过 FileWorkspace 桩的触发按钮驱动
 * onSave/onFormat/onContentChange/onClose；通过 fake invoke 控制响应时序与 stale 场景。
 */
import { describe, expect, test } from 'vitest';
import { fireEvent, screen } from '@testing-library/react';

import {
  buildDependencyContextValue,
  buildLocalProject,
  buildProjectsContextValue,
  buildSession,
  buildWorktree,
  createDeferred,
  flushMacrotasks,
  renderWorkbench,
  selectInspectorTab,
  setInvokeHandler,
  waitFor,
  waitForInvoke,
} from './testing/workbenchTestHarness';
import type { WorkbenchFileNode, WorkbenchOpenFile } from '@/lib/types';

async function settle(): Promise<void> {
  await flushMacrotasks();
}

/** 构造一个可编辑文本文件的 opened payload。 */
function buildOpenedText(path: string, content: string, baseHash: string): WorkbenchOpenFile {
  return {
    metadata: { name: path.split('/').pop() ?? path, path, kind: 'file', size: content.length, modifiedAt: '2026-07-01T00:00:00.000Z' },
    detectedType: 'code',
    capabilities: {
      canPreview: false,
      canEdit: true,
      canFormat: false,
      mustValidateBeforeSave: false,
      defaultMode: 'editor',
      availableModes: ['editor', 'source'],
    },
    text: { content, baseHash, baseModifiedAt: '2026-07-01T00:00:00.000Z' },
    image: null,
    csv: null,
    sqlite: null,
    truncated: false,
    notice: null,
  };
}

describe('Workbench files domain (characterization)', () => {
  test('directory stale response is ignored when a newer refresh resolves first', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const session = buildSession();
    const staleNodes: WorkbenchFileNode[] = [
      { name: 'STALE_DIR', path: 'STALE_DIR', kind: 'dir', size: null, modifiedAt: null, children: null },
    ];
    const freshNodes: WorkbenchFileNode[] = [
      { name: 'README.md', path: 'README.md', kind: 'file', size: 1, modifiedAt: null, children: null },
    ];

    // 第一次 list_workbench_dir（root）返回 deferred；点“刷新文件”触发第二次 root 请求，立即返回 fresh。
    const firstRootDeferred = createDeferred<WorkbenchFileNode[]>();
    let firstRootCall = true;
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return [worktree];
        case 'list_workbench_sessions':
          return [session];
        case 'list_workbench_git_commits':
          return [];
        case 'list_workbench_dir':
          if ((call.args.path ?? '') === '' && firstRootCall) {
            firstRootCall = false;
            return firstRootDeferred.promise;
          }
          return freshNodes;
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: project.id }),
      buildDependencyContextValue(),
    );
    await waitForInvoke('list_workbench_dir');
    // inspector 默认 tab 为 Git 历史；文件域测试显式切回 files tab 再驱动文件树/工具条 UI。
    await selectInspectorTab('files');

    // 点击“刷新文件”触发第二次 root 请求（立即返回 freshNodes）。
    fireEvent.click(screen.getByRole('button', { name: '刷新文件' }));
    await waitFor(() => {
      const treePanel = document.querySelector('[class*="treePanel"]');
      if (!treePanel?.textContent?.includes('README.md')) {
        throw new Error('fresh root not rendered');
      }
    });

    // 现在解析 stale deferred；STALE_DIR 不应出现在文件树。
    firstRootDeferred.resolve(staleNodes);
    await settle();

    const treePanel = document.querySelector('[class*="treePanel"]');
    expect(treePanel?.textContent).toContain('README.md');
    expect(treePanel?.textContent).not.toContain('STALE_DIR');
  });

  test('opening a file creates a tab and switches workspace to files view', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const session = buildSession();
    const readme: WorkbenchFileNode = {
      name: 'README.md',
      path: 'README.md',
      kind: 'file',
      size: 12,
      modifiedAt: null,
      children: null,
    };
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return [worktree];
        case 'list_workbench_sessions':
          return [session];
        case 'list_workbench_git_commits':
          return [];
        case 'list_workbench_dir':
          return [readme];
        case 'get_workbench_path_info':
          return { name: 'README.md', path: 'README.md', kind: 'file', size: 12, modifiedAt: '2026-07-01T00:00:00.000Z' };
        case 'open_workbench_file':
          return buildOpenedText('README.md', '# hello', 'hash-1');
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: project.id }),
      buildDependencyContextValue(),
    );
    await waitForInvoke('list_workbench_dir');
    // inspector 默认 tab 为 Git 历史；文件域测试显式切回 files tab 再驱动文件树/工具条 UI。
    await selectInspectorTab('files');
    // 等待文件树渲染出 README.md 节点。
    await waitFor(() => {
      const treeButton = screen.queryAllByRole('button', { name: 'README.md' });
      if (treeButton.length === 0) throw new Error('README.md tree node not rendered');
    });

    // 点击 README.md 节点打开文件。
    fireEvent.click(screen.getByRole('button', { name: 'README.md' }));
    await waitForInvoke('open_workbench_file');

    // workspace 切到 files 视图：FileWorkspace 桩出现，且 tab-count >= 1。
    const fileWorkspace = await waitFor(() => {
      const node = screen.queryByTestId('workbench-file-workspace');
      if (!node || node.getAttribute('data-tab-count') === '0') {
        throw new Error('file workspace not mounted with tabs');
      }
      return node;
    });
    expect(fileWorkspace.getAttribute('data-tab-count')).toBe('1');
    // terminal layer 应被隐藏（workspaceView !== 'terminal'）。
    const terminalLayer = document.querySelector('[class*="terminalLayer"]');
    expect(terminalLayer?.getAttribute('data-hidden')).toBe('true');
  });

  test('open file stale response is ignored when a newer open request supersedes it', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const session = buildSession();
    const fileA: WorkbenchFileNode = { name: 'a.txt', path: 'a.txt', kind: 'file', size: 1, modifiedAt: null, children: null };
    const fileB: WorkbenchFileNode = { name: 'b.txt', path: 'b.txt', kind: 'file', size: 1, modifiedAt: null, children: null };
    const openADeferred = createDeferred<WorkbenchOpenFile>();

    let openACalled = false;
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return [worktree];
        case 'list_workbench_sessions':
          return [session];
        case 'list_workbench_git_commits':
          return [];
        case 'list_workbench_dir':
          return [fileA, fileB];
        case 'get_workbench_path_info':
          return { name: String(call.args.path), path: String(call.args.path), kind: 'file', size: 1, modifiedAt: null };
        case 'open_workbench_file':
          if (call.args.path === 'a.txt') {
            if (!openACalled) {
              openACalled = true;
              return openADeferred.promise;
            }
            return buildOpenedText('a.txt', 'A', 'hash-a');
          }
          return buildOpenedText('b.txt', 'B', 'hash-b');
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: project.id }),
      buildDependencyContextValue(),
    );
    await waitForInvoke('list_workbench_dir');
    // inspector 默认 tab 为 Git 历史；文件域测试显式切回 files tab 再驱动文件树/工具条 UI。
    await selectInspectorTab('files');
    await waitFor(() => {
      if (screen.queryAllByRole('button', { name: 'a.txt' }).length === 0) {
        throw new Error('a.txt not rendered');
      }
    });

    // 先点 a.txt（deferred 挂起），再点 b.txt（立即返回，成为最新）。
    fireEvent.click(screen.getByRole('button', { name: 'a.txt' }));
    fireEvent.click(screen.getByRole('button', { name: 'b.txt' }));
    await waitForInvoke('open_workbench_file', 2);

    // 现在 resolve a.txt 的 stale 响应；该响应不应激活 a.txt tab，active tab 应仍是 b.txt。
    openADeferred.resolve(buildOpenedText('a.txt', 'A-stale', 'hash-a-stale'));
    await settle();

    // FileWorkspace 的 active-tab-id 应反映 b.txt（worktreeId 路径前缀）。
    await waitFor(() => {
      const node = screen.getByTestId('workbench-file-workspace');
      if (!node.getAttribute('data-active-tab-id')?.endsWith(':b.txt')) {
        throw new Error(`active tab not b.txt, got ${node.getAttribute('data-active-tab-id')}`);
      }
    });
  });

  test('dirty tab close prompts confirm; cancelling keeps the tab open', async () => {
    const project = buildLocalProject();
    const worktree = buildWorktree();
    const session = buildSession();
    const readme: WorkbenchFileNode = { name: 'README.md', path: 'README.md', kind: 'file', size: 1, modifiedAt: null, children: null };
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return [worktree];
        case 'list_workbench_sessions':
          return [session];
        case 'list_workbench_git_commits':
          return [];
        case 'list_workbench_dir':
          return [readme];
        case 'get_workbench_path_info':
          return { name: 'README.md', path: 'README.md', kind: 'file', size: 1, modifiedAt: null };
        case 'open_workbench_file':
          return buildOpenedText('README.md', 'original', 'hash-1');
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: project.id }),
      buildDependencyContextValue(),
    );
    await waitForInvoke('list_workbench_dir');
    // inspector 默认 tab 为 Git 历史；文件域测试显式切回 files tab 再驱动文件树/工具条 UI。
    await selectInspectorTab('files');
    await waitFor(() => {
      if (screen.queryAllByRole('button', { name: 'README.md' }).length === 0) {
        throw new Error('README.md not rendered');
      }
    });

    // 打开 README.md，然后通过桩的 edit 按钮触发 onContentChange → tab 变 dirty。
    fireEvent.click(screen.getByRole('button', { name: 'README.md' }));
    await waitForInvoke('open_workbench_file');
    // 等 active tab 真正落到 README.md，再触发 content-change。
    await waitFor(() => {
      const node = screen.getByTestId('workbench-file-workspace');
      if (!node.getAttribute('data-active-tab-id')?.endsWith(':README.md')) {
        throw new Error('active tab not README.md');
      }
    });
    fireEvent.click(screen.getByTestId('workbench-file-workspace-content-change'));
    await settle();

    // confirm 返回 false（取消关闭）：tab 应保留。
    const original = window.confirm;
    let confirmCalls = 0;
    window.confirm = (): boolean => {
      confirmCalls += 1;
      return false;
    };
    try {
      fireEvent.click(screen.getByTestId('workbench-file-workspace-close'));
      await settle();
      // 至少触发了一次 confirm（说明 tab 确实 dirty），且取消后 tab 仍存在。
      expect(confirmCalls).toBe(1);
      const node = screen.getByTestId('workbench-file-workspace');
      expect(node.getAttribute('data-tab-count')).toBe('1');
    } finally {
      window.confirm = original;
    }
  });
});
