/**
 * Transfer 原生路径选择适配器单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   桌面发送路径必须来自 Tauri dialog/drag，且跨平台路径为不透明 UTF-8，
 *   普通浏览器测试环境不得注册原生 drop 监听。
 *
 * Code Logic（这个测试做什么）:
 *   mock plugin-dialog / webview；断言 open 参数、cancel→null、Windows 路径原样、
 *   drop.paths 原样转发、非 Tauri 环境 no-op unsubscribe。
 */

// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';

const openMock = vi.fn();
const onDragDropEventMock = vi.fn();
const getCurrentWebviewMock = vi.fn(() => ({
  onDragDropEvent: onDragDropEventMock,
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: (...args: unknown[]) => openMock(...args),
}));

vi.mock('@tauri-apps/api/webview', () => ({
  getCurrentWebview: () => getCurrentWebviewMock(),
}));

import { pickTransferFile, subscribeTransferFileDrops } from './transferFileSelection';

type TauriWindow = Window & {
  __TAURI_INTERNALS__?: { transformCallback?: (...args: unknown[]) => unknown };
};

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需模拟桌面 Tauri runtime 与普通浏览器两种边界。
 *
 * Code Logic（这个函数做什么）:
 *   设置或清除 window.__TAURI_INTERNALS__.transformCallback。
 */
function setTauriInternals(enabled: boolean): void {
  const win = window as TauriWindow;
  if (enabled) {
    win.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
    };
    return;
  }
  delete win.__TAURI_INTERNALS__;
}

describe('pickTransferFile', () => {
  beforeEach(() => {
    openMock.mockReset();
  });

  afterEach(() => {
    setTauriInternals(false);
  });

  test('opens native dialog with multiple:false and directory:false', async () => {
    openMock.mockResolvedValueOnce('/Users/hans/file.txt');

    const path = await pickTransferFile();

    expect(openMock).toHaveBeenCalledWith({ multiple: false, directory: false });
    expect(path).toBe('/Users/hans/file.txt');
  });

  test('returns null when user cancels dialog', async () => {
    openMock.mockResolvedValueOnce(null);

    await expect(pickTransferFile()).resolves.toBeNull();
  });

  test('returns Windows path unchanged as opaque UTF-8', async () => {
    const windowsPath = 'C:\\Users\\hans\\Desktop\\报告 1.txt';
    openMock.mockResolvedValueOnce(windowsPath);

    const path = await pickTransferFile();

    expect(path).toBe(windowsPath);
    expect(path).not.toContain('/');
  });

  test('rejects non-string dialog payloads without rewriting', async () => {
    openMock.mockResolvedValueOnce(['/tmp/a.txt', '/tmp/b.txt']);

    await expect(pickTransferFile()).resolves.toBeNull();
  });
});

describe('subscribeTransferFileDrops', () => {
  beforeEach(() => {
    onDragDropEventMock.mockReset();
    getCurrentWebviewMock.mockClear();
    setTauriInternals(false);
  });

  afterEach(() => {
    setTauriInternals(false);
  });

  test('non-Tauri environment returns no-op unsubscribe without registering listener', async () => {
    setTauriInternals(false);
    const onPaths = vi.fn();

    const unsubscribe = await subscribeTransferFileDrops(onPaths);
    unsubscribe();

    expect(getCurrentWebviewMock).not.toHaveBeenCalled();
    expect(onDragDropEventMock).not.toHaveBeenCalled();
    expect(onPaths).not.toHaveBeenCalled();
  });

  test('forwards native drop.paths unchanged', async () => {
    setTauriInternals(true);
    const windowsPaths = [
      'C:\\Users\\hans\\a.txt',
      'C:\\Users\\hans\\folder\\报告 2.txt',
    ];
    let handler:
      | ((event: { payload: { type: string; paths?: string[] } }) => void)
      | undefined;
    const unlisten = vi.fn();
    onDragDropEventMock.mockImplementationOnce(async (cb: typeof handler) => {
      handler = cb;
      return unlisten;
    });

    const onPaths = vi.fn();
    const unsubscribe = await subscribeTransferFileDrops(onPaths);

    expect(getCurrentWebviewMock).toHaveBeenCalledTimes(1);
    expect(onDragDropEventMock).toHaveBeenCalledTimes(1);
    expect(handler).toBeTypeOf('function');

    handler?.({
      payload: {
        type: 'drop',
        paths: windowsPaths,
      },
    });

    expect(onPaths).toHaveBeenCalledWith(windowsPaths);
    expect(onPaths.mock.calls[0]?.[0]).toEqual(windowsPaths);

    // enter/over/leave 不触发 onPaths
    handler?.({ payload: { type: 'enter', paths: windowsPaths } });
    handler?.({ payload: { type: 'over' } });
    handler?.({ payload: { type: 'leave' } });
    expect(onPaths).toHaveBeenCalledTimes(1);

    unsubscribe();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });
});
