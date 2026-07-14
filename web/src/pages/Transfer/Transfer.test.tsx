/**
 * Transfer 页面 send/cancel 旅程测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   桌面传输必须用原生路径发送/取消，展示 basename，轮询失败保留列表，
 *   且不得渲染后端未支持的 pause/retry/open。
 *
 * Code Logic（这个测试做什么）:
 *   mock transfer/devices API、path adapter 与 useVisibilityPolling；
 *   覆盖选文件、多文件 drop、发送成功/失败、双击 send/cancel 只调一次 API、
 *   取消 busy/error、刷新失败保列表。
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import type { Device, TransferTask } from '@/lib/types';

const listDevicesMock = vi.fn();
const listTransfersMock = vi.fn();
const sendTransferMock = vi.fn();
const cancelTransferMock = vi.fn();
const pickTransferFileMock = vi.fn();
const subscribeTransferFileDropsMock = vi.fn();

type PollingTask = () => Promise<void>;
type RunNowOptions = { force?: boolean };
type RunNowFn = ((options?: RunNowOptions) => Promise<void>) & {
  mock: { calls: unknown[][] };
};
const pollingRegistrations: Array<{
  taskRef: { current: PollingTask };
  options: { intervalMs: number };
  runNow: RunNowFn;
  started: boolean;
}> = [];

vi.mock('@/api/devices', () => ({
  devicesApi: {
    list: (...args: unknown[]) => listDevicesMock(...args),
  },
}));

vi.mock('@/api/transfer', () => ({
  transferApi: {
    list: (...args: unknown[]) => listTransfersMock(...args),
    send: (...args: unknown[]) => sendTransferMock(...args),
    cancel: (...args: unknown[]) => cancelTransferMock(...args),
  },
}));

vi.mock('./transferFileSelection', () => ({
  pickTransferFile: (...args: unknown[]) => pickTransferFileMock(...args),
  subscribeTransferFileDrops: (...args: unknown[]) => subscribeTransferFileDropsMock(...args),
}));

vi.mock('@/hooks/useVisibilityPolling', () => ({
  useVisibilityPolling: (task: PollingTask, options: { intervalMs: number }) => {
    let entry = pollingRegistrations.find((item) => item.options.intervalMs === options.intervalMs);
    if (!entry) {
      const taskRef = { current: task };
      const runNow = vi.fn(async () => {
        await taskRef.current();
      }) as unknown as RunNowFn;
      entry = { taskRef, options, runNow, started: false };
      pollingRegistrations.push(entry);
    } else {
      entry.taskRef.current = task;
    }
    // 仅首次注册时立即执行，对齐 runImmediately 且避免每 render 重入
    if (!entry.started) {
      entry.started = true;
      void entry.runNow();
    }
    return { runNow: entry.runNow, inFlight: false };
  },
}));

import { Transfer } from './Transfer';

const deviceA: Device = {
  id: 'device-a',
  name: 'MacBook Pro',
  address: '192.168.1.10',
  port: 62116,
  status: 'online',
};

/**
 * Business Logic（为什么需要这个函数）:
 *   多个用例共享最小合法任务 DTO。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的 TransferTask。
 */
function buildTask(overrides: Partial<TransferTask> = {}): TransferTask {
  return {
    id: 'task-1',
    fileName: 'report.txt',
    filePath: '/Users/hans/report.txt',
    fileSize: 2048,
    direction: 'send',
    status: 'transferring',
    progress: 0.5,
    peerDeviceName: 'MacBook Pro',
    speed: 1024,
    startedAt: '2026-07-13T00:00:00.000Z',
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   契约测试需统一 i18n 挂载。
 *
 * Code Logic（这个函数做什么）:
 *   用 I18nextProvider 渲染 Transfer 页面。
 */
function renderTransfer() {
  return render(
    <I18nextProvider i18n={i18n}>
      <Transfer />
    </I18nextProvider>,
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需定位任务/设备轮询注册，以便手动触发刷新。
 *
 * Code Logic（这个函数做什么）:
 *   按 intervalMs 从 pollingRegistrations 取对应条目。
 */
function getPolling(intervalMs: number) {
  const entry = pollingRegistrations.find((item) => item.options.intervalMs === intervalMs);
  if (!entry) {
    throw new Error(`missing polling registration for ${intervalMs}`);
  }
  return {
    options: entry.options,
    runNow: entry.runNow,
    task: () => entry.taskRef.current(),
  };
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

beforeEach(() => {
  pollingRegistrations.length = 0;
  listDevicesMock.mockReset();
  listTransfersMock.mockReset();
  sendTransferMock.mockReset();
  cancelTransferMock.mockReset();
  pickTransferFileMock.mockReset();
  subscribeTransferFileDropsMock.mockReset();

  listDevicesMock.mockResolvedValue([deviceA]);
  listTransfersMock.mockResolvedValue([]);
  subscribeTransferFileDropsMock.mockResolvedValue(() => undefined);
});

afterEach(() => {
  cleanup();
});

describe('Transfer page journey', () => {
  test('registers 3000ms task polling and 5000ms device polling', async () => {
    renderTransfer();

    await waitFor(() => {
      expect(pollingRegistrations.map((item) => item.options.intervalMs).sort()).toEqual([
        3000, 5000,
      ]);
    });
  });

  test('displays basename only after picking a Windows path', async () => {
    const windowsPath = 'C:\\Users\\hans\\Desktop\\报告 1.txt';
    pickTransferFileMock.mockResolvedValueOnce(windowsPath);
    renderTransfer();

    await waitFor(() => {
      expect(screen.getByRole('combobox', { name: '选择目标设备' })).toBeTruthy();
    });

    fireEvent.click(screen.getByRole('button', { name: '浏览…' }));

    await waitFor(() => {
      expect(screen.getByText('已选择：报告 1.txt')).toBeTruthy();
    });

    expect(screen.queryByText(windowsPath)).toBeNull();
    expect(screen.getByRole('button', { name: /发送「报告 1.txt」/ })).toBeTruthy();
  });

  test('Enter and Space on dropzone open native picker', async () => {
    pickTransferFileMock.mockResolvedValue('/Users/hans/docs/a.txt');
    renderTransfer();

    await waitFor(() => {
      expect(screen.getByLabelText('拖拽文件到此处或点击选择')).toBeTruthy();
    });

    const dropzone = screen.getByLabelText('拖拽文件到此处或点击选择');
    fireEvent.keyDown(dropzone, { key: 'Enter' });
    await waitFor(() => {
      expect(pickTransferFileMock).toHaveBeenCalledTimes(1);
    });

    fireEvent.keyDown(dropzone, { key: ' ' });
    await waitFor(() => {
      expect(pickTransferFileMock).toHaveBeenCalledTimes(2);
    });
  });

  test('native drop chooses first path only and shows localized notice', async () => {
    let onPaths: ((paths: string[]) => void) | undefined;
    subscribeTransferFileDropsMock.mockImplementationOnce(async (cb: (paths: string[]) => void) => {
      onPaths = cb;
      return () => undefined;
    });

    renderTransfer();

    await waitFor(() => {
      expect(onPaths).toBeTypeOf('function');
    });

    act(() => {
      onPaths?.(['C:\\Users\\hans\\a.txt', 'C:\\Users\\hans\\b.txt']);
    });

    await waitFor(() => {
      expect(screen.getByText('已选择：a.txt')).toBeTruthy();
      expect(screen.getByRole('alert').textContent).toContain('本轮仅发送第一个文件，其余已忽略。');
    });

    expect(screen.queryByText(/b\.txt/)).toBeNull();
  });

  test('sending disables button; success clears selection and refreshes tasks', async () => {
    const windowsPath = 'C:\\Users\\hans\\Desktop\\report.txt';
    pickTransferFileMock.mockResolvedValueOnce(windowsPath);
    let resolveSend: ((value: unknown) => void) | undefined;
    sendTransferMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveSend = resolve;
        }),
    );
    listTransfersMock.mockImplementation(async () => {
      if (sendTransferMock.mock.calls.length > 0) {
        return [
          buildTask({
            id: 'transfer-1',
            fileName: 'report.txt',
            filePath: windowsPath,
            status: 'pending',
            progress: 0,
            speed: undefined,
          }),
        ];
      }
      return [];
    });

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByRole('button', { name: '浏览…' })).toBeTruthy();
    });

    fireEvent.click(screen.getByRole('button', { name: '浏览…' }));
    await waitFor(() => {
      const sendBtn = screen.getByRole('button', { name: /发送「report\.txt」/ });
      expect(sendBtn.hasAttribute('disabled')).toBe(false);
    });

    fireEvent.click(screen.getByRole('button', { name: /发送「report\.txt」/ }));

    await waitFor(() => {
      expect(sendTransferMock).toHaveBeenCalledWith('device-a', windowsPath);
      const sendBtn = screen.getByRole('button', { name: /发送「report\.txt」/ });
      expect(sendBtn.hasAttribute('disabled')).toBe(true);
      expect(sendBtn.getAttribute('aria-busy')).toBe('true');
    });

    await act(async () => {
      resolveSend?.({
        accepted: true,
        deviceId: 'device-a',
        filePath: windowsPath,
        id: 'transfer-1',
      });
    });

    await waitFor(() => {
      expect(screen.getByText('拖拽文件到此处 或 点击选择')).toBeTruthy();
      expect(screen.getByText('report.txt')).toBeTruthy();
    });

    const taskPolling = getPolling(3000);
    expect(taskPolling.runNow).toHaveBeenCalled();
  });

  test('send failure retains selection and shows alert', async () => {
    pickTransferFileMock.mockResolvedValueOnce('/Users/hans/keep-me.txt');
    sendTransferMock.mockRejectedValueOnce(new Error('device offline'));

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByRole('button', { name: '浏览…' })).toBeTruthy();
    });

    fireEvent.click(screen.getByRole('button', { name: '浏览…' }));
    await waitFor(() => {
      const sendBtn = screen.getByRole('button', { name: /发送「keep-me\.txt」/ });
      expect(sendBtn.hasAttribute('disabled')).toBe(false);
    });

    fireEvent.click(screen.getByRole('button', { name: /发送「keep-me\.txt」/ }));

    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toContain('发送失败：device offline');
      const sendBtn = screen.getByRole('button', { name: /发送「keep-me\.txt」/ });
      expect(sendBtn.hasAttribute('disabled')).toBe(false);
      expect(screen.getByText('已选择：keep-me.txt')).toBeTruthy();
    });
  });

  test('double-click send invokes send API once', async () => {
    const windowsPath = 'C:\\Users\\hans\\Desktop\\dbl-send.txt';
    pickTransferFileMock.mockResolvedValueOnce(windowsPath);
    let resolveSend: ((value: unknown) => void) | undefined;
    sendTransferMock.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveSend = resolve;
        }),
    );

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByRole('button', { name: '浏览…' })).toBeTruthy();
    });

    fireEvent.click(screen.getByRole('button', { name: '浏览…' }));
    await waitFor(() => {
      const sendBtn = screen.getByRole('button', { name: /发送「dbl-send\.txt」/ });
      expect(sendBtn.hasAttribute('disabled')).toBe(false);
    });

    const sendBtn = screen.getByRole('button', { name: /发送「dbl-send\.txt」/ });
    fireEvent.click(sendBtn);
    fireEvent.click(sendBtn);

    await waitFor(() => {
      expect(sendTransferMock).toHaveBeenCalledTimes(1);
      expect(sendTransferMock).toHaveBeenCalledWith('device-a', windowsPath);
    });

    await act(async () => {
      resolveSend?.({
        accepted: true,
        deviceId: 'device-a',
        filePath: windowsPath,
        id: 'transfer-dbl',
      });
    });
  });

  test('cancel failure keeps task and shows row alert', async () => {
    listTransfersMock.mockResolvedValue([
      buildTask({
        id: 'task-fail',
        status: 'pending',
        fileName: 'fail.bin',
        progress: 0,
        speed: undefined,
      }),
    ]);
    cancelTransferMock.mockRejectedValueOnce(new Error('cancel rejected'));

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByText('fail.bin')).toBeTruthy();
    });

    fireEvent.click(screen.getByRole('button', { name: '取消' }));

    await waitFor(() => {
      expect(cancelTransferMock).toHaveBeenCalledWith('task-fail');
      expect(screen.getByRole('alert').textContent).toContain('取消失败：cancel rejected');
      expect(screen.getByText('fail.bin')).toBeTruthy();
    });
  });

  test('cancel success refreshes task list via runNow', async () => {
    listTransfersMock.mockResolvedValue([
      buildTask({
        id: 'task-ok',
        status: 'transferring',
        fileName: 'ok.bin',
      }),
    ]);
    cancelTransferMock.mockResolvedValueOnce({ ok: true, id: 'task-ok' });

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByText('ok.bin')).toBeTruthy();
    });

    const runNowBefore = getPolling(3000).runNow.mock.calls.length;
    fireEvent.click(screen.getByRole('button', { name: '取消' }));

    await waitFor(() => {
      expect(cancelTransferMock).toHaveBeenCalledWith('task-ok');
      expect(getPolling(3000).runNow.mock.calls.length).toBeGreaterThan(runNowBefore);
      const lastCall = getPolling(3000).runNow.mock.calls.at(-1);
      expect(lastCall?.[0]).toEqual({ force: true });
    });
  });

  test('double-click cancel invokes cancel API once', async () => {
    listTransfersMock.mockResolvedValue([
      buildTask({
        id: 'task-dbl',
        status: 'pending',
        fileName: 'dbl.bin',
        progress: 0,
        speed: undefined,
      }),
    ]);
    let resolveCancel: ((value: { ok: true; id: string }) => void) | undefined;
    cancelTransferMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveCancel = resolve;
        }),
    );

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByText('dbl.bin')).toBeTruthy();
    });

    const cancelBtn = screen.getByRole('button', { name: '取消' });
    fireEvent.click(cancelBtn);
    fireEvent.click(cancelBtn);

    await waitFor(() => {
      expect(cancelTransferMock).toHaveBeenCalledTimes(1);
    });

    await act(async () => {
      resolveCancel?.({ ok: true, id: 'task-dbl' });
    });
  });

  test('refresh failure preserves existing task list', async () => {
    listTransfersMock.mockResolvedValue([
      buildTask({ id: 'keep-1', fileName: 'keep-alive.txt', status: 'completed', progress: 1 }),
    ]);

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByText('keep-alive.txt')).toBeTruthy();
    });

    listTransfersMock.mockRejectedValueOnce(new Error('network down'));

    await act(async () => {
      await getPolling(3000).task();
    });

    expect(screen.getByText('keep-alive.txt')).toBeTruthy();
    expect(screen.getByRole('status').textContent).toContain('任务列表加载失败：network down');
  });

  test('does not render pause/retry/open for listed tasks', async () => {
    listTransfersMock.mockResolvedValue([
      buildTask({ id: 't-transferring', status: 'transferring', fileName: 'a.bin' }),
      buildTask({
        id: 't-failed',
        status: 'failed',
        fileName: 'b.bin',
        progress: 0.1,
        errorMessage: 'x',
        speed: undefined,
      }),
      buildTask({
        id: 't-completed',
        status: 'completed',
        fileName: 'c.bin',
        progress: 1,
        speed: undefined,
      }),
    ]);

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByText('a.bin')).toBeTruthy();
      expect(screen.getByText('b.bin')).toBeTruthy();
      expect(screen.getByText('c.bin')).toBeTruthy();
    });

    expect(screen.queryByRole('button', { name: '暂停' })).toBeNull();
    expect(screen.queryByRole('button', { name: '重试' })).toBeNull();
    expect(screen.queryByRole('button', { name: '打开' })).toBeNull();
    expect(screen.getAllByRole('button', { name: '取消' }).length).toBeGreaterThan(0);
  });

  test('dialog cancel keeps previous selection', async () => {
    pickTransferFileMock
      .mockResolvedValueOnce('/Users/hans/first.txt')
      .mockResolvedValueOnce(null);

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByRole('button', { name: '浏览…' })).toBeTruthy();
    });

    fireEvent.click(screen.getByRole('button', { name: '浏览…' }));
    await waitFor(() => {
      expect(screen.getByText('已选择：first.txt')).toBeTruthy();
    });

    fireEvent.click(screen.getByRole('button', { name: '浏览…' }));
    await waitFor(() => {
      expect(pickTransferFileMock).toHaveBeenCalledTimes(2);
    });

    expect(screen.getByText('已选择：first.txt')).toBeTruthy();
  });
});
