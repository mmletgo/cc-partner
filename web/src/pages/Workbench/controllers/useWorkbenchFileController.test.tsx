// @vitest-environment jsdom
/**
 * useWorkbenchFileController 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   在 controller 抽取后，文件域的目录加载/children 缓存、选中信息、tab 去重打开、dirty 关闭确认/丢弃/保存、
 *   baseHash 冲突、保存/格式化、image/CSV/SQLite/HTML/Markdown 模式状态、create file/dir、rename、delete、
 *   copy path、project/worktree stale 响应守卫以及 resetForContext 必须独立可测。这些行为原先散落在
 *   Workbench.tsx 多处 state/handler/effect，本测试覆盖抽出后仍保持原有契约。
 *
 * Code Logic（这个测试做什么）:
 *   - 用 vi.mock 接管 @/api/workbench 的 files API；
 *   - 通过 @testing-library/react 的 renderHook 把 controller 挂在 React 树中；
 *   - 用 rerender 模拟项目/worktree 切换；用 act 触发回调；通过 fake isCurrentProject 模拟 stale 场景；
 *   - 断言 rootNodes / childrenByPath / fileTabs / activeFileTabId / fileError / fileNotice 等状态与 files API 调用日志。
 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';

import { useWorkbenchFileController } from './useWorkbenchFileController';
import type { UseWorkbenchFileControllerParams } from './useWorkbenchFileController';
import type {
  WorkbenchFileNode,
  WorkbenchFormatResult,
  WorkbenchOpenFile,
  WorkbenchPathInfo,
  WorkbenchSaveTextResult,
  WorkbenchSqlitePreview,
} from '@/lib/types';

/* ---------------------------------------------------------------------------
 * vi.mock — workbench files API
 *
 * Business Logic: controller 单元测试不应触发真实 Tauri invoke；用一个可断言的 fake 记录所有 files 调用，
 * 并允许测试动态设置返回值或抛出错误。
 * ------------------------------------------------------------------------- */

interface FakeFilesApi {
  listDir: ReturnType<typeof vi.fn>;
  info: ReturnType<typeof vi.fn>;
  open: ReturnType<typeof vi.fn>;
  saveText: ReturnType<typeof vi.fn>;
  formatStructured: ReturnType<typeof vi.fn>;
  previewSqlite: ReturnType<typeof vi.fn>;
  previewHtmlAsset: ReturnType<typeof vi.fn>;
  createFile: ReturnType<typeof vi.fn>;
  createDir: ReturnType<typeof vi.fn>;
  renamePath: ReturnType<typeof vi.fn>;
  deletePath: ReturnType<typeof vi.fn>;
}

const fakeFilesApi = vi.hoisted<FakeFilesApi>(() => ({
  listDir: vi.fn(async () => [] as WorkbenchFileNode[]),
  info: vi.fn(async () => ({}) as WorkbenchPathInfo),
  open: vi.fn(async () => ({}) as WorkbenchOpenFile),
  saveText: vi.fn(async () => ({}) as WorkbenchSaveTextResult),
  formatStructured: vi.fn(async () => ({ formatted: '' }) as WorkbenchFormatResult),
  previewSqlite: vi.fn(async () => ({}) as WorkbenchSqlitePreview),
  previewHtmlAsset: vi.fn(async () => ({}) as never),
  createFile: vi.fn(async () => ({}) as WorkbenchPathInfo),
  createDir: vi.fn(async () => ({}) as WorkbenchPathInfo),
  renamePath: vi.fn(async () => ({}) as WorkbenchPathInfo),
  deletePath: vi.fn(async () => ({ ok: true, path: '' })),
}));

vi.mock('@/api/workbench', () => ({
  workbenchApi: {
    files: fakeFilesApi,
  },
}));

/* ---------------------------------------------------------------------------
 * Fixture builders
 * ------------------------------------------------------------------------- */

function buildFileNode(overrides: Partial<WorkbenchFileNode> = {}): WorkbenchFileNode {
  return {
    name: 'README.md',
    path: 'README.md',
    kind: 'file',
    size: 12,
    modifiedAt: null,
    children: null,
    ...overrides,
  };
}

function buildPathInfo(overrides: Partial<WorkbenchPathInfo> = {}): WorkbenchPathInfo {
  return {
    name: 'README.md',
    path: 'README.md',
    kind: 'file',
    size: 12,
    modifiedAt: '2026-07-01T00:00:00.000Z',
    ...overrides,
  };
}

function buildOpenedText(
  path: string,
  content: string,
  baseHash: string,
  detectedType: WorkbenchOpenFile['detectedType'] = 'code',
): WorkbenchOpenFile {
  return {
    metadata: {
      name: path.split('/').pop() ?? path,
      path,
      kind: 'file',
      size: content.length,
      modifiedAt: '2026-07-01T00:00:00.000Z',
    },
    detectedType,
    capabilities: {
      canPreview: false,
      canEdit: true,
      canFormat: detectedType === 'json' || detectedType === 'toml' || detectedType === 'yaml',
      mustValidateBeforeSave:
        detectedType === 'json' || detectedType === 'toml' || detectedType === 'yaml',
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

/* ---------------------------------------------------------------------------
 * renderHook helper
 * ------------------------------------------------------------------------- */

interface ControllerProps {
  activeProjectId: string | null;
  activeWorktreeId: string | null;
  remoteWriteDisabled: boolean;
  markRequestFailure: (projectId: string, error: unknown) => void;
  markRequestSuccess: (projectId: string) => void;
  isCurrentProject: (projectId: string) => boolean;
  requestWorkspaceView: (view: 'terminal' | 'files') => void;
  requestHideAutomationConsole: () => void;
  displayErrorMessage?: (error: unknown, fallback: string, desktopUnavailable: string) => string;
  desktopUnavailableMessage?: string;
  translateFileError?: (key: import('./useWorkbenchFileController').WorkbenchFileErrorKey) => string;
  translateFileMessage?: (
    key:
      | 'saved'
      | 'formatted'
      | 'pathCopied'
      | 'confirmCloseDirtyFile'
      | 'confirmDeleteDirtyFiles'
      | 'confirmDeletePath',
    vars?: Record<string, unknown>,
  ) => string;
}

function renderController(props: Partial<ControllerProps> = {}) {
  const merged = baseControllerProps(props);
  return renderHook(
    (currentProps: ControllerProps) =>
      useWorkbenchFileController(currentProps as UseWorkbenchFileControllerParams),
    { initialProps: merged },
  );
}

function baseControllerProps(overrides: Partial<ControllerProps> = {}): ControllerProps {
  return {
    activeProjectId: 'project-1',
    activeWorktreeId: 'worktree-main',
    remoteWriteDisabled: false,
    markRequestFailure: vi.fn(),
    markRequestSuccess: vi.fn(),
    isCurrentProject: () => true,
    requestWorkspaceView: vi.fn(),
    requestHideAutomationConsole: vi.fn(),
    displayErrorMessage: undefined,
    desktopUnavailableMessage: 'desktop unavailable',
    translateFileError: undefined,
    translateFileMessage: (key, vars) =>
      vars ? `${key}:${JSON.stringify(vars)}` : (key as string),
    ...overrides,
  };
}

/** 等待 pending microtask / Promise.then 落地。 */
async function flushMicrotasks(rounds = 6): Promise<void> {
  for (let i = 0; i < rounds; i += 1) {
    await Promise.resolve();
  }
}

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true, advanceTimeDelta: 1 });
  // Business Logic: 每个 test 独立；reset 所有 mock 调用记录与 mockXxxOnce 队列，并恢复默认 async 实现，
  // 避免前一个测试的 mockResolvedValueOnce/mockResolvedValue 残留泄漏到下一个测试。
  vi.resetAllMocks();
  fakeFilesApi.listDir.mockResolvedValue([]);
  fakeFilesApi.info.mockResolvedValue(buildPathInfo());
  fakeFilesApi.open.mockResolvedValue({} as WorkbenchOpenFile);
  fakeFilesApi.saveText.mockResolvedValue({} as WorkbenchSaveTextResult);
  fakeFilesApi.formatStructured.mockResolvedValue({ formatted: '' });
  fakeFilesApi.previewSqlite.mockResolvedValue({} as WorkbenchSqlitePreview);
  fakeFilesApi.previewHtmlAsset.mockResolvedValue({} as never);
  fakeFilesApi.createFile.mockResolvedValue(buildPathInfo());
  fakeFilesApi.createDir.mockResolvedValue(buildPathInfo());
  fakeFilesApi.renamePath.mockResolvedValue(buildPathInfo());
  fakeFilesApi.deletePath.mockResolvedValue({ ok: true, path: '' });
});

afterEach(() => {
  vi.useRealTimers();
  cleanup();
});

/* ---------------------------------------------------------------------------
 * loadDir / children cache
 * ------------------------------------------------------------------------- */

describe('useWorkbenchFileController — loadDir / children cache', () => {
  test('loadDir root stores rootNodes, marks request success', async () => {
    const readme = buildFileNode();
    const markSuccess = vi.fn();
    fakeFilesApi.listDir.mockResolvedValueOnce([readme]);

    const { result } = renderController({ markRequestSuccess: markSuccess });

    await act(async () => {
      await result.current.loadDir('');
      await flushMicrotasks();
    });

    expect(result.current.rootNodes).toEqual([readme]);
    expect(result.current.fileLoadingPath).toBeNull();
    expect(markSuccess).toHaveBeenCalledWith('project-1');
  });

  test('loadDir child path stores childrenByPath entry', async () => {
    const child = buildFileNode({ name: 'src/a.ts', path: 'src/a.ts' });
    fakeFilesApi.listDir.mockResolvedValueOnce([child]);

    const { result } = renderController();

    await act(async () => {
      await result.current.loadDir('src');
      await flushMicrotasks();
    });

    expect(result.current.childrenByPath['src']).toEqual([child]);
    expect(result.current.rootNodes).toEqual([]);
  });

  test('loadDir stale project response is ignored', async () => {
    const stale = buildFileNode({ name: 'STALE', path: 'STALE' });
    fakeFilesApi.listDir.mockResolvedValueOnce([stale]);

    const { result } = renderController({
      isCurrentProject: () => false,
    });

    await act(async () => {
      await result.current.loadDir('');
      await flushMicrotasks();
    });

    expect(result.current.rootNodes).toEqual([]);
  });

  test('loadDir stale seq response is ignored when a newer refresh resolves first', async () => {
    // 同一 project/worktree/path 下发起两次 loadDir；第一次（stale）后 resolve，结果不应写入。
    const staleNodes = [buildFileNode({ name: 'STALE_DIR', path: 'STALE_DIR', kind: 'dir' })];
    const freshNodes = [buildFileNode({ name: 'README.md', path: 'README.md' })];

    let firstCall = true;
    const firstPromise = Promise.resolve(staleNodes);
    fakeFilesApi.listDir.mockImplementation(() => {
      if (firstCall) {
        firstCall = false;
        return firstPromise;
      }
      return Promise.resolve(freshNodes);
    });

    const { result } = renderController();

    await act(async () => {
      // 第一次请求挂起（已 resolve 但我们手动控制 await 顺序）
      const first = result.current.loadDir('');
      // 第二次请求立即 resolve 为 fresh
      await result.current.loadDir('');
      await first;
      await flushMicrotasks();
    });

    expect(result.current.rootNodes).toEqual(freshNodes);
  });

  test('loadDir surfaces fileError and marks failure on throw', async () => {
    const markFailure = vi.fn();
    fakeFilesApi.listDir.mockRejectedValueOnce(new Error('boom'));

    const { result } = renderController({ markRequestFailure: markFailure });

    await act(async () => {
      await result.current.loadDir('');
      await flushMicrotasks();
    });

    expect(result.current.fileError).toContain('boom');
    expect(result.current.fileLoadingPath).toBeNull();
    expect(markFailure).toHaveBeenCalledWith('project-1', expect.any(Error));
  });
});

/* ---------------------------------------------------------------------------
 * path info / selectedPath / selectedInfo
 * ------------------------------------------------------------------------- */

describe('useWorkbenchFileController — selectNode / loadPathInfo', () => {
  test('selectNode sets selectedPath/selectedInfo/renameName and triggers loadPathInfo', async () => {
    const info = buildPathInfo({ name: 'a.txt', path: 'a.txt', size: 1 });
    fakeFilesApi.info.mockResolvedValueOnce(info);

    const { result } = renderController();

    await act(async () => {
      result.current.handleSelectNode(buildFileNode({ name: 'a.txt', path: 'a.txt' }));
      await flushMicrotasks();
    });

    expect(result.current.selectedPath).toBe('a.txt');
    expect(result.current.selectedInfo).toEqual(info);
    expect(result.current.renameName).toBe('a.txt');
  });

  test('loadPathInfo stale project response is ignored', async () => {
    const info = buildPathInfo({ name: 'a.txt', path: 'a.txt', size: 99 });
    fakeFilesApi.info.mockResolvedValueOnce(info);

    const { result } = renderController({
      isCurrentProject: () => false,
    });

    await act(async () => {
      result.current.handleSelectNode(buildFileNode({ name: 'a.txt', path: 'a.txt' }));
      await flushMicrotasks();
    });

    // selectedInfo 由 selectNode 同步写入 node metadata（size=12, modifiedAt=null）；
    // loadPathInfo 的 stale 回写（size=99）被忽略，因此保留 node metadata。
    expect(result.current.selectedInfo?.size).toBe(12);
    expect(result.current.selectedInfo?.modifiedAt).toBeNull();
  });
});

/* ---------------------------------------------------------------------------
 * open file / tab dedupe / activate
 * ------------------------------------------------------------------------- */

describe('useWorkbenchFileController — open file / tab dedupe', () => {
  test('handleOpenFile creates a tab, activates it, and switches workspace to files', async () => {
    const opened = buildOpenedText('README.md', '# hello', 'hash-1');
    fakeFilesApi.open.mockResolvedValueOnce(opened);
    const requestView = vi.fn();

    const { result } = renderController({ requestWorkspaceView: requestView });

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'README.md', path: 'README.md' }));
      await flushMicrotasks();
    });

    expect(result.current.fileTabs).toHaveLength(1);
    expect(result.current.fileTabs[0].path).toBe('README.md');
    expect(result.current.fileTabs[0].dirty).toBe(false);
    expect(result.current.activeFileTabId).toBe('worktree-main:README.md');
    expect(requestView).toHaveBeenCalledWith('files');
  });

  test('openFileByPath opens relative path and rejects traversal', async () => {
    const opened = buildOpenedText('WORKFLOW.md', '---\n', 'hash-wf');
    fakeFilesApi.open.mockResolvedValueOnce(opened);
    const { result } = renderController();

    await act(async () => {
      const ok = await result.current.openFileByPath('WORKFLOW.md');
      await flushMicrotasks();
      expect(ok).toBe(true);
    });
    expect(fakeFilesApi.open).toHaveBeenCalledWith('project-1', 'WORKFLOW.md', 'worktree-main');
    expect(result.current.fileTabs[0]?.path).toBe('WORKFLOW.md');

    await act(async () => {
      const rejected = await result.current.openFileByPath('../secret');
      expect(rejected).toBe(false);
    });
  });

  test('handleOpenFile dedupes already-open tab and preserves dirty/content/mode', async () => {
    const openedFirst = buildOpenedText('README.md', 'original', 'hash-1');
    const openedSecond = buildOpenedText('README.md', 'fresh-from-disk', 'hash-2');
    fakeFilesApi.open.mockResolvedValueOnce(openedFirst);
    fakeFilesApi.open.mockResolvedValueOnce(openedSecond);

    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'README.md', path: 'README.md' }));
      await flushMicrotasks();
    });
    // 把 tab 改成 dirty + 自定义 mode + content
    act(() => {
      result.current.handleFileModeChange('worktree-main:README.md', 'source');
      result.current.handleFileContentChange('worktree-main:README.md', 'edited');
    });

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'README.md', path: 'README.md' }));
      await flushMicrotasks();
    });

    expect(result.current.fileTabs).toHaveLength(1);
    const tab = result.current.fileTabs[0];
    // dirty tab 的保存基线 (text) 必须保留，不能被 fresh opened 覆盖。
    expect(tab.opened.text?.baseHash).toBe('hash-1');
    expect(tab.dirty).toBe(true);
    expect(tab.content).toBe('edited');
    expect(tab.mode).toBe('source');
  });

  test('handleOpenFile stale response (superseded by newer open) is ignored', async () => {
    const openedA = buildOpenedText('a.txt', 'A-stale', 'hash-a-stale');
    const openedB = buildOpenedText('b.txt', 'B', 'hash-b');
    fakeFilesApi.open.mockResolvedValueOnce(openedA);
    fakeFilesApi.open.mockResolvedValueOnce(openedB);

    const { result } = renderController();

    const nodeA = buildFileNode({ name: 'a.txt', path: 'a.txt' });
    const nodeB = buildFileNode({ name: 'b.txt', path: 'b.txt' });

    await act(async () => {
      // 先发起 a.txt（pending），再发起 b.txt（最新）
      const first = result.current.handleOpenFile(nodeA);
      await result.current.handleOpenFile(nodeB);
      await first;
      await flushMicrotasks();
    });

    // active tab 应是 b.txt，a.txt 的 stale 响应未激活
    expect(result.current.activeFileTabId).toBe('worktree-main:b.txt');
    expect(result.current.fileTabs.map((tab) => tab.path)).toEqual(['b.txt']);
  });

  test('handleActivateFileTab switches active and workspace to files', async () => {
    const opened = buildOpenedText('README.md', 'hi', 'h');
    fakeFilesApi.open.mockResolvedValueOnce(opened);
    const requestView = vi.fn();
    const requestHide = vi.fn();

    const { result } = renderController({
      requestWorkspaceView: requestView,
      requestHideAutomationConsole: requestHide,
    });

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'README.md', path: 'README.md' }));
      await flushMicrotasks();
    });

    requestView.mockClear();
    requestHide.mockClear();

    act(() => {
      result.current.handleActivateFileTab('worktree-main:README.md');
    });

    expect(result.current.activeFileTabId).toBe('worktree-main:README.md');
    expect(requestView).toHaveBeenCalledWith('files');
    expect(requestHide).toHaveBeenCalled();
  });
});

/* ---------------------------------------------------------------------------
 * dirty tab close cancel / discard / save
 * ------------------------------------------------------------------------- */

describe('useWorkbenchFileController — close tab dirty confirm', () => {
  test('closing a dirty tab prompts confirm; cancelling keeps the tab open', async () => {
    const opened = buildOpenedText('README.md', 'original', 'hash-1');
    fakeFilesApi.open.mockResolvedValueOnce(opened);

    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'README.md', path: 'README.md' }));
      await flushMicrotasks();
    });
    act(() => {
      result.current.handleFileContentChange('worktree-main:README.md', 'edited');
    });

    const original = window.confirm;
    const confirmSpy = vi.fn(() => false);
    window.confirm = confirmSpy;
    try {
      act(() => {
        result.current.handleCloseFileTab('worktree-main:README.md');
      });
    } finally {
      window.confirm = original;
    }

    expect(confirmSpy).toHaveBeenCalledTimes(1);
    expect(result.current.fileTabs).toHaveLength(1);
    expect(result.current.activeFileTabId).toBe('worktree-main:README.md');
  });

  test('closing a dirty tab with confirm=true removes the tab and falls back to terminal', async () => {
    const opened = buildOpenedText('README.md', 'original', 'hash-1');
    fakeFilesApi.open.mockResolvedValueOnce(opened);
    const requestView = vi.fn();

    const { result } = renderController({ requestWorkspaceView: requestView });

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'README.md', path: 'README.md' }));
      await flushMicrotasks();
    });
    act(() => {
      result.current.handleFileContentChange('worktree-main:README.md', 'edited');
    });

    const original = window.confirm;
    window.confirm = () => true;
    try {
      act(() => {
        result.current.handleCloseFileTab('worktree-main:README.md');
      });
    } finally {
      window.confirm = original;
    }

    expect(result.current.fileTabs).toHaveLength(0);
    expect(result.current.activeFileTabId).toBeNull();
    // 关闭最后一个 tab 时应请求切回 terminal 视图。
    expect(requestView).toHaveBeenCalledWith('terminal');
  });

  test('closing a non-dirty tab skips confirm and removes it', async () => {
    const opened = buildOpenedText('README.md', 'hi', 'h');
    fakeFilesApi.open.mockResolvedValueOnce(opened);

    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'README.md', path: 'README.md' }));
      await flushMicrotasks();
    });

    const confirmSpy = vi.fn(() => true);
    const original = window.confirm;
    window.confirm = confirmSpy;
    try {
      act(() => {
        result.current.handleCloseFileTab('worktree-main:README.md');
      });
    } finally {
      window.confirm = original;
    }

    expect(confirmSpy).not.toHaveBeenCalled();
    expect(result.current.fileTabs).toHaveLength(0);
  });
});

/* ---------------------------------------------------------------------------
 * save / format / baseHash conflict
 * ------------------------------------------------------------------------- */

describe('useWorkbenchFileController — save / format / baseHash', () => {
  test('handleSaveFileTab writes new baseHash, clears dirty, refreshes parent dir', async () => {
    const opened = buildOpenedText('pkg.json', '{"name":"a"}', 'hash-1', 'json');
    fakeFilesApi.open.mockResolvedValueOnce(opened);
    const saved: WorkbenchSaveTextResult = {
      metadata: buildPathInfo({ name: 'pkg.json', path: 'pkg.json' }),
      baseHash: 'hash-2',
      baseModifiedAt: '2026-07-02T00:00:00.000Z',
    };
    fakeFilesApi.saveText.mockResolvedValueOnce(saved);
    fakeFilesApi.listDir.mockResolvedValue([]);

    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'pkg.json', path: 'pkg.json' }));
      await flushMicrotasks();
    });
    act(() => {
      result.current.handleFileContentChange('worktree-main:pkg.json', '{"name":"b"}');
    });

    await act(async () => {
      await result.current.handleSaveFileTab('worktree-main:pkg.json');
      await flushMicrotasks();
    });

    const tab = result.current.fileTabs[0];
    expect(tab.dirty).toBe(false);
    expect(tab.opened.text?.baseHash).toBe('hash-2');
    expect(tab.content).toBe('{"name":"b"}');
    expect(result.current.fileNotice).toBeTruthy();
    expect(fakeFilesApi.saveText).toHaveBeenCalledWith(
      'project-1',
      'pkg.json',
      '{"name":"b"}',
      'hash-1',
      'worktree-main',
    );
  });

  test('handleSaveFileTab preserves in-flight edits made during save (keeps dirty)', async () => {
    const opened = buildOpenedText('pkg.json', '{"name":"a"}', 'hash-1', 'json');
    fakeFilesApi.open.mockResolvedValueOnce(opened);
    const saved: WorkbenchSaveTextResult = {
      metadata: buildPathInfo({ name: 'pkg.json', path: 'pkg.json' }),
      baseHash: 'hash-2',
      baseModifiedAt: '2026-07-02T00:00:00.000Z',
    };
    // 用 deferred 控制 saveText 的 resolve 时机，确保“保存期间”有窗口让用户继续编辑。
    let resolveSave!: (value: WorkbenchSaveTextResult) => void;
    fakeFilesApi.saveText.mockImplementationOnce(
      () => new Promise<WorkbenchSaveTextResult>((resolve) => (resolveSave = resolve)),
    );
    fakeFilesApi.listDir.mockResolvedValue([]);

    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'pkg.json', path: 'pkg.json' }));
      await flushMicrotasks();
    });
    act(() => {
      result.current.handleFileContentChange('worktree-main:pkg.json', '{"name":"b"}');
    });

    let savingPromise!: Promise<void>;
    await act(async () => {
      savingPromise = result.current.handleSaveFileTab('worktree-main:pkg.json');
      // 保存请求已发出但尚未 resolve；此时用户继续编辑（仍是合法 JSON）。
      result.current.handleFileContentChange('worktree-main:pkg.json', '{"name":"b","x":1}');
      await flushMicrotasks();
    });

    // 现在 resolve 保存响应。
    await act(async () => {
      resolveSave(saved);
      await savingPromise;
      await flushMicrotasks();
    });

    const tab = result.current.fileTabs[0];
    expect(tab.dirty).toBe(true);
    expect(tab.content).toBe('{"name":"b","x":1}');
    // baseHash 仍然推进到最新保存基线
    expect(tab.opened.text?.baseHash).toBe('hash-2');
  });

  test('handleSaveFileTab blocks save when baseHash missing and surfaces fileError', async () => {
    // 没有 text（image 文件等），baseHash 为 undefined
    const opened: WorkbenchOpenFile = {
      ...buildOpenedText('README.md', '', 'hash-1'),
      text: null,
      detectedType: 'image',
      image: { dataUrl: 'data:image/png;base64,', mime: 'image/png', width: 1, height: 1 },
    };
    fakeFilesApi.open.mockResolvedValueOnce(opened);

    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'README.md', path: 'README.md' }));
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleSaveFileTab('worktree-main:README.md');
      await flushMicrotasks();
    });

    expect(fakeFilesApi.saveText).not.toHaveBeenCalled();
    expect(result.current.fileError).toBeTruthy();
  });

  test('handleSaveFileTab blocks save on invalid JSON and surfaces validation error', async () => {
    const opened = buildOpenedText('pkg.json', '{invalid', 'hash-1', 'json');
    fakeFilesApi.open.mockResolvedValueOnce(opened);

    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'pkg.json', path: 'pkg.json' }));
      await flushMicrotasks();
    });
    act(() => {
      result.current.handleFileContentChange('worktree-main:pkg.json', '{invalid json');
    });

    await act(async () => {
      await result.current.handleSaveFileTab('worktree-main:pkg.json');
      await flushMicrotasks();
    });

    expect(fakeFilesApi.saveText).not.toHaveBeenCalled();
    expect(result.current.fileError).toContain(':');
  });

  test('handleSaveFileTab respects remoteWriteDisabled', async () => {
    const opened = buildOpenedText('pkg.json', '{"a":1}', 'hash-1', 'json');
    fakeFilesApi.open.mockResolvedValueOnce(opened);

    const { result } = renderController({ remoteWriteDisabled: true });

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'pkg.json', path: 'pkg.json' }));
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleSaveFileTab('worktree-main:pkg.json');
      await flushMicrotasks();
    });

    expect(fakeFilesApi.saveText).not.toHaveBeenCalled();
  });

  test('handleFormatFileTab updates content and marks dirty when seq still latest', async () => {
    const opened = buildOpenedText('pkg.json', '{"a":1}', 'hash-1', 'json');
    fakeFilesApi.open.mockResolvedValueOnce(opened);
    fakeFilesApi.formatStructured.mockResolvedValueOnce({ formatted: '{\n  "a": 1\n}\n' });

    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'pkg.json', path: 'pkg.json' }));
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleFormatFileTab('worktree-main:pkg.json');
      await flushMicrotasks();
    });

    const tab = result.current.fileTabs[0];
    expect(tab.content).toBe('{\n  "a": 1\n}\n');
    expect(tab.dirty).toBe(true);
    expect(result.current.fileNotice).toBeTruthy();
  });

  test('handleFormatFileTab skips non-structured types', async () => {
    const opened = buildOpenedText('README.md', '# hi', 'hash-1', 'code');
    fakeFilesApi.open.mockResolvedValueOnce(opened);

    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'README.md', path: 'README.md' }));
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleFormatFileTab('worktree-main:README.md');
      await flushMicrotasks();
    });

    expect(fakeFilesApi.formatStructured).not.toHaveBeenCalled();
  });

  test('handleFormatFileTab respects remoteWriteDisabled', async () => {
    const opened = buildOpenedText('pkg.json', '{"a":1}', 'hash-1', 'json');
    fakeFilesApi.open.mockResolvedValueOnce(opened);

    const { result } = renderController({ remoteWriteDisabled: true });

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'pkg.json', path: 'pkg.json' }));
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleFormatFileTab('worktree-main:pkg.json');
      await flushMicrotasks();
    });

    expect(fakeFilesApi.formatStructured).not.toHaveBeenCalled();
  });
});

/* ---------------------------------------------------------------------------
 * SQLite preview mode state
 * ------------------------------------------------------------------------- */

describe('useWorkbenchFileController — sqlite preview', () => {
  test('handleSelectSqliteTable replaces tab.opened.sqlite when seq still latest', async () => {
    const initialSqlite: WorkbenchSqlitePreview = {
      tables: ['t1', 't2'],
      selectedTable: 't1',
      columns: ['id'],
      rows: [['1']],
      truncated: false,
    };
    const opened: WorkbenchOpenFile = {
      ...buildOpenedText('data.db', '', 'hash-1', 'sqlite'),
      text: null,
      detectedType: 'sqlite',
      sqlite: initialSqlite,
    };
    fakeFilesApi.open.mockResolvedValueOnce(opened);
    const nextSqlite: WorkbenchSqlitePreview = {
      tables: ['t1', 't2'],
      selectedTable: 't2',
      columns: ['name'],
      rows: [['n1']],
      truncated: false,
    };
    fakeFilesApi.previewSqlite.mockResolvedValueOnce(nextSqlite);

    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'data.db', path: 'data.db' }));
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleSelectSqliteTable('worktree-main:data.db', 't2');
      await flushMicrotasks();
    });

    expect(result.current.fileTabs[0].opened.sqlite).toEqual(nextSqlite);
    expect(fakeFilesApi.previewSqlite).toHaveBeenCalledWith(
      'project-1',
      'data.db',
      't2',
      100,
      'worktree-main',
    );
  });

  test('handleSelectSqliteTable stale project response is ignored', async () => {
    const initialSqlite: WorkbenchSqlitePreview = {
      tables: ['t1'],
      selectedTable: 't1',
      columns: ['id'],
      rows: [['1']],
      truncated: false,
    };
    const opened: WorkbenchOpenFile = {
      ...buildOpenedText('data.db', '', 'hash-1', 'sqlite'),
      text: null,
      detectedType: 'sqlite',
      sqlite: initialSqlite,
    };
    fakeFilesApi.open.mockResolvedValueOnce(opened);
    const nextSqlite: WorkbenchSqlitePreview = {
      ...initialSqlite,
      selectedTable: 't1',
      columns: ['other'],
    };
    fakeFilesApi.previewSqlite.mockResolvedValueOnce(nextSqlite);

    // 用 rerender 模拟：发起 sqlite 预览请求后切换 project，响应到达时 activeProjectIdRef 已变。
    const { result, rerender } = renderController({ activeProjectId: 'project-1' });

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'data.db', path: 'data.db' }));
      await flushMicrotasks();
    });

    // 发起预览，但在响应落地前切到另一个 project。
    const pending = result.current.handleSelectSqliteTable('worktree-main:data.db', 't1');
    rerender(baseControllerProps({ activeProjectId: 'project-other' }));
    await act(async () => {
      await pending;
      await flushMicrotasks();
    });

    expect(result.current.fileTabs[0].opened.sqlite).toEqual(initialSqlite);
  });
});

/* ---------------------------------------------------------------------------
 * create file / dir
 * ------------------------------------------------------------------------- */

describe('useWorkbenchFileController — create file / dir', () => {
  test('handleCreateEntry file creates under selectedParentPath, selects, expands parent, reloads dir', async () => {
    const created = buildPathInfo({ name: 'new.txt', path: 'src/new.txt' });
    fakeFilesApi.createFile.mockResolvedValueOnce(created);
    fakeFilesApi.listDir.mockResolvedValue([]);
    fakeFilesApi.info.mockResolvedValue(created);

    const { result } = renderController();

    // 先选中 src 目录（dir），parentPath 解析为 src
    await act(async () => {
      result.current.handleSelectNode(
        buildFileNode({ name: 'src', path: 'src', kind: 'dir', size: null }),
      );
      await flushMicrotasks();
    });
    act(() => {
      result.current.setNewEntryName('new.txt');
    });

    await act(async () => {
      await result.current.handleCreateEntry('file');
      await flushMicrotasks();
    });

    expect(fakeFilesApi.createFile).toHaveBeenCalledWith(
      'project-1',
      'src',
      'new.txt',
      'worktree-main',
    );
    expect(result.current.selectedPath).toBe('src/new.txt');
    expect(result.current.selectedInfo).toEqual(created);
    expect(result.current.newEntryName).toBe('');
    expect(result.current.expandedPaths.has('src')).toBe(true);
  });

  test('handleCreateEntry respects remoteWriteDisabled', async () => {
    const { result } = renderController({ remoteWriteDisabled: true });
    act(() => {
      result.current.setNewEntryName('x');
    });

    await act(async () => {
      await result.current.handleCreateEntry('file');
      await flushMicrotasks();
    });

    expect(fakeFilesApi.createFile).not.toHaveBeenCalled();
  });

  test('handleCreateEntry skips when newEntryName empty', async () => {
    const { result } = renderController();

    await act(async () => {
      await result.current.handleCreateEntry('file');
      await flushMicrotasks();
    });

    expect(fakeFilesApi.createFile).not.toHaveBeenCalled();
  });
});

/* ---------------------------------------------------------------------------
 * rename
 * ------------------------------------------------------------------------- */

describe('useWorkbenchFileController — rename', () => {
  test('handleRenamePath renames tab path/id, keeps content/dirty, refreshes parent dir', async () => {
    const opened = buildOpenedText('src/a.ts', 'content-a', 'hash-a');
    fakeFilesApi.open.mockResolvedValueOnce(opened);
    const infoOnSelect = buildPathInfo({ name: 'a.ts', path: 'src/a.ts' });
    fakeFilesApi.info.mockResolvedValueOnce(infoOnSelect);
    const renamed = buildPathInfo({ name: 'b.ts', path: 'src/b.ts' });
    fakeFilesApi.renamePath.mockResolvedValueOnce(renamed);
    fakeFilesApi.listDir.mockResolvedValue([]);

    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'a.ts', path: 'src/a.ts' }));
      await flushMicrotasks();
    });
    act(() => {
      result.current.handleFileContentChange('worktree-main:src/a.ts', 'edited');
    });
    // 选中目标路径
    await act(async () => {
      result.current.handleSelectNode(buildFileNode({ name: 'a.ts', path: 'src/a.ts' }));
      await flushMicrotasks();
    });
    act(() => {
      result.current.setRenameName('b.ts');
    });

    await act(async () => {
      await result.current.handleRenamePath();
      await flushMicrotasks();
    });

    expect(fakeFilesApi.renamePath).toHaveBeenCalledWith(
      'project-1',
      'src/a.ts',
      'b.ts',
      'worktree-main',
    );
    expect(result.current.fileTabs).toHaveLength(1);
    const tab = result.current.fileTabs[0];
    expect(tab.id).toBe('worktree-main:src/b.ts');
    expect(tab.path).toBe('src/b.ts');
    // dirty/content 保留
    expect(tab.dirty).toBe(true);
    expect(tab.content).toBe('edited');
    // active 跟随改名后的 id
    expect(result.current.activeFileTabId).toBe('worktree-main:src/b.ts');
    expect(result.current.selectedPath).toBe('src/b.ts');
  });

  test('handleRenamePath respects remoteWriteDisabled', async () => {
    const { result } = renderController({ remoteWriteDisabled: true });
    await act(async () => {
      result.current.handleSelectNode(buildFileNode({ name: 'a.ts', path: 'a.ts' }));
      await flushMicrotasks();
    });
    act(() => {
      result.current.setRenameName('b.ts');
    });

    await act(async () => {
      await result.current.handleRenamePath();
      await flushMicrotasks();
    });

    expect(fakeFilesApi.renamePath).not.toHaveBeenCalled();
  });
});

/* ---------------------------------------------------------------------------
 * delete
 * ------------------------------------------------------------------------- */

describe('useWorkbenchFileController — delete', () => {
  test('handleDeletePath prompts dirty + path confirm, removes affected tabs, reloads parent', async () => {
    const opened = buildOpenedText('a.txt', 'x', 'h');
    fakeFilesApi.open.mockResolvedValueOnce(opened);
    fakeFilesApi.info.mockResolvedValueOnce(buildPathInfo({ name: 'a.txt', path: 'a.txt' }));
    fakeFilesApi.deletePath.mockResolvedValueOnce({ ok: true, path: 'a.txt' });
    fakeFilesApi.listDir.mockResolvedValue([]);

    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'a.txt', path: 'a.txt' }));
      await flushMicrotasks();
    });
    act(() => {
      result.current.handleFileContentChange('worktree-main:a.txt', 'edited');
    });
    await act(async () => {
      result.current.handleSelectNode(buildFileNode({ name: 'a.txt', path: 'a.txt' }));
      await flushMicrotasks();
    });

    const confirms: boolean[] = [true, true];
    let callIndex = 0;
    const original = window.confirm;
    window.confirm = () => confirms[callIndex++] ?? true;
    try {
      await act(async () => {
        await result.current.handleDeletePath();
        await flushMicrotasks();
      });
    } finally {
      window.confirm = original;
    }

    expect(fakeFilesApi.deletePath).toHaveBeenCalledWith('project-1', 'a.txt', 'worktree-main');
    expect(result.current.fileTabs).toHaveLength(0);
    expect(result.current.activeFileTabId).toBeNull();
    expect(result.current.selectedPath).toBeNull();
    expect(result.current.selectedInfo).toBeNull();
  });

  test('handleDeletePath cancelling dirty confirm keeps the tab', async () => {
    const opened = buildOpenedText('a.txt', 'x', 'h');
    fakeFilesApi.open.mockResolvedValueOnce(opened);

    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'a.txt', path: 'a.txt' }));
      await flushMicrotasks();
    });
    act(() => {
      result.current.handleFileContentChange('worktree-main:a.txt', 'edited');
    });
    await act(async () => {
      result.current.handleSelectNode(buildFileNode({ name: 'a.txt', path: 'a.txt' }));
      await flushMicrotasks();
    });

    const original = window.confirm;
    window.confirm = () => false;
    try {
      await act(async () => {
        await result.current.handleDeletePath();
        await flushMicrotasks();
      });
    } finally {
      window.confirm = original;
    }

    expect(fakeFilesApi.deletePath).not.toHaveBeenCalled();
    expect(result.current.fileTabs).toHaveLength(1);
  });

  test('handleDeletePath respects remoteWriteDisabled', async () => {
    const { result } = renderController({ remoteWriteDisabled: true });
    await act(async () => {
      result.current.handleSelectNode(buildFileNode({ name: 'a.txt', path: 'a.txt' }));
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleDeletePath();
      await flushMicrotasks();
    });

    expect(fakeFilesApi.deletePath).not.toHaveBeenCalled();
  });
});

/* ---------------------------------------------------------------------------
 * copy path
 * ------------------------------------------------------------------------- */

describe('useWorkbenchFileController — copy path', () => {
  test('handleCopySelectedPath writes path to clipboard and sets fileNotice', async () => {
    const writeText = vi.fn(async () => undefined);
    Object.assign(navigator, {
      clipboard: { writeText },
    });
    fakeFilesApi.info.mockResolvedValueOnce(buildPathInfo({ name: 'a.txt', path: 'a.txt' }));

    const { result } = renderController();
    await act(async () => {
      result.current.handleSelectNode(buildFileNode({ name: 'a.txt', path: 'a.txt' }));
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleCopySelectedPath();
      await flushMicrotasks();
    });

    expect(writeText).toHaveBeenCalledWith('a.txt');
    expect(result.current.fileNotice).toBeTruthy();
    expect(result.current.fileError).toBeNull();
  });

  test('handleCopySelectedPath surfaces fileError on clipboard failure', async () => {
    const writeText = vi.fn(async () => {
      throw new Error('denied');
    });
    Object.assign(navigator, {
      clipboard: { writeText },
    });

    const { result } = renderController();
    await act(async () => {
      result.current.handleSelectNode(buildFileNode({ name: 'a.txt', path: 'a.txt' }));
      await flushMicrotasks();
    });

    await act(async () => {
      await result.current.handleCopySelectedPath();
      await flushMicrotasks();
    });

    expect(result.current.fileError).toContain('denied');
  });
});

/* ---------------------------------------------------------------------------
 * toggle node / expandedPaths
 * ------------------------------------------------------------------------- */

describe('useWorkbenchFileController — toggle node', () => {
  test('handleToggleNode expands dir and triggers loadDir when children not cached', async () => {
    fakeFilesApi.listDir.mockResolvedValueOnce([
      buildFileNode({ name: 'child', path: 'src/child' }),
    ]);

    const { result } = renderController();

    await act(async () => {
      result.current.handleToggleNode(
        buildFileNode({ name: 'src', path: 'src', kind: 'dir', size: null }),
      );
      await flushMicrotasks();
    });

    expect(result.current.expandedPaths.has('src')).toBe(true);
    expect(fakeFilesApi.listDir).toHaveBeenCalledWith('project-1', 'src', 'worktree-main');
  });

  test('handleToggleNode collapses expanded dir without loading', async () => {
    fakeFilesApi.listDir.mockResolvedValue([]);
    const { result } = renderController();

    await act(async () => {
      result.current.handleToggleNode(
        buildFileNode({ name: 'src', path: 'src', kind: 'dir', size: null }),
      );
      await flushMicrotasks();
    });
    fakeFilesApi.listDir.mockClear();

    await act(async () => {
      result.current.handleToggleNode(
        buildFileNode({ name: 'src', path: 'src', kind: 'dir', size: null }),
      );
      await flushMicrotasks();
    });

    expect(result.current.expandedPaths.has('src')).toBe(false);
    expect(fakeFilesApi.listDir).not.toHaveBeenCalled();
  });

  test('handleToggleNode ignores file nodes', async () => {
    const { result } = renderController();

    await act(async () => {
      result.current.handleToggleNode(buildFileNode({ name: 'a.txt', path: 'a.txt' }));
      await flushMicrotasks();
    });

    expect(result.current.expandedPaths.size).toBe(0);
  });
});

/* ---------------------------------------------------------------------------
 * resetForContext / guardDirtyContextChange bridge
 * ------------------------------------------------------------------------- */

describe('useWorkbenchFileController — resetForContext bridge', () => {
  test('resetForContext clears all file domain state', async () => {
    const opened = buildOpenedText('a.txt', 'x', 'h');
    fakeFilesApi.open.mockResolvedValueOnce(opened);
    fakeFilesApi.listDir.mockResolvedValue([]);

    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'a.txt', path: 'a.txt' }));
      await flushMicrotasks();
    });
    act(() => {
      result.current.setNewEntryName('pending');
      result.current.setRenameName('renamed');
    });
    // 制造一些状态
    await act(async () => {
      result.current.handleToggleNode(
        buildFileNode({ name: 'src', path: 'src', kind: 'dir', size: null }),
      );
      await flushMicrotasks();
    });

    act(() => {
      result.current.resetForContext(null, null);
    });

    expect(result.current.rootNodes).toEqual([]);
    expect(result.current.childrenByPath).toEqual({});
    expect(result.current.expandedPaths.size).toBe(0);
    expect(result.current.selectedPath).toBeNull();
    expect(result.current.selectedInfo).toBeNull();
    expect(result.current.fileTabs).toEqual([]);
    expect(result.current.activeFileTabId).toBeNull();
    expect(result.current.fileError).toBeNull();
    expect(result.current.fileNotice).toBeNull();
    expect(result.current.fileSaving).toBe(false);
    // newEntryName / renameName 不属于跨 context 必须清空的“选中态”，但原 Workbench.tsx
    // reset 也没有清空 newEntryName；保持一致不清空。
  });

  test('resetForContext ignores pending stale open responses after reset', async () => {
    // 发起 open，但在响应回来前 reset；reset 后即使响应到达也不应激活 tab。
    fakeFilesApi.open.mockResolvedValueOnce(buildOpenedText('a.txt', 'A', 'h'));

    const { result } = renderController();

    let pending: Promise<void>;
    act(() => {
      pending = result.current.handleOpenFile(buildFileNode({ name: 'a.txt', path: 'a.txt' }));
    });

    act(() => {
      result.current.resetForContext('project-1', 'worktree-main');
    });

    await act(async () => {
      await pending!;
      await flushMicrotasks();
    });

    expect(result.current.fileTabs).toHaveLength(0);
    expect(result.current.activeFileTabId).toBeNull();
  });
});

describe('useWorkbenchFileController — guardDirtyContextChange bridge', () => {
  test('returns true when no dirty tabs', async () => {
    const { result } = renderController();

    let guardResult: boolean = false;
    await act(async () => {
      guardResult = await result.current.guardDirtyContextChange();
    });

    expect(guardResult).toBe(true);
  });

  test('returns true when dirty tabs and user confirms', async () => {
    const opened = buildOpenedText('a.txt', 'x', 'h');
    fakeFilesApi.open.mockResolvedValueOnce(opened);
    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'a.txt', path: 'a.txt' }));
      await flushMicrotasks();
    });
    act(() => {
      result.current.handleFileContentChange('worktree-main:a.txt', 'edited');
    });

    const original = window.confirm;
    window.confirm = () => true;
    let guardResult = false;
    try {
      await act(async () => {
        guardResult = await result.current.guardDirtyContextChange();
      });
    } finally {
      window.confirm = original;
    }

    expect(guardResult).toBe(true);
  });

  test('returns false when dirty tabs and user cancels', async () => {
    const opened = buildOpenedText('a.txt', 'x', 'h');
    fakeFilesApi.open.mockResolvedValueOnce(opened);
    const { result } = renderController();

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'a.txt', path: 'a.txt' }));
      await flushMicrotasks();
    });
    act(() => {
      result.current.handleFileContentChange('worktree-main:a.txt', 'edited');
    });

    const original = window.confirm;
    window.confirm = () => false;
    let guardResult = true;
    try {
      await act(async () => {
        guardResult = await result.current.guardDirtyContextChange();
      });
    } finally {
      window.confirm = original;
    }

    expect(guardResult).toBe(false);
    // 取消后 tab 仍存在
    expect(result.current.fileTabs).toHaveLength(1);
  });
});

/* ---------------------------------------------------------------------------
 * content / mode change remote-write guard
 * ------------------------------------------------------------------------- */

describe('useWorkbenchFileController — content / mode change guards', () => {
  test('handleFileContentChange respects remoteWriteDisabled after rerender', async () => {
    const opened = buildOpenedText('a.txt', 'x', 'h');
    fakeFilesApi.open.mockResolvedValueOnce(opened);
    const { result, rerender } = renderController({ remoteWriteDisabled: false });

    await act(async () => {
      await result.current.handleOpenFile(buildFileNode({ name: 'a.txt', path: 'a.txt' }));
      await flushMicrotasks();
    });

    // 切到只读后再编辑，content 不应被改动
    rerender(baseControllerProps({ remoteWriteDisabled: true }));
    const before = result.current.fileTabs[0].content;
    act(() => {
      result.current.handleFileContentChange('worktree-main:a.txt', 'should-be-ignored');
    });
    expect(result.current.fileTabs[0].content).toBe(before);
    expect(result.current.fileTabs[0].dirty).toBe(false);
  });
});
