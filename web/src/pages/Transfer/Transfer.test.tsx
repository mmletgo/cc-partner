/**
 * Transfer 页面 send/cancel/recovery 旅程测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   桌面传输必须用原生路径发送/取消，展示 basename，轮询失败保留列表，
 *   且按 phase/action 矩阵只渲染合法 recovery 动作（resume/retry/open/reveal）。
 *
 * Code Logic（这个测试做什么）:
 *   mock transfer/devices API、path adapter 与 useVisibilityPolling；
 *   覆盖选文件、多文件 drop、发送成功/失败、双击 send/cancel 只调一次 API、
 *   取消 busy/error、刷新失败保列表、action matrix 与 uncertain 对账。
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import { MOBILE_INBOX_DEVICE_ID } from '@/lib/mobileInbox';
import type { Device, TransferTask } from '@/lib/types';

const listDevicesMock = vi.fn();
const listTransfersMock = vi.fn();
const sendTransferMock = vi.fn();
const cancelTransferMock = vi.fn();
const retryTransferMock = vi.fn();
const resumeTransferMock = vi.fn();
const getOperationMock = vi.fn();
const openTransferMock = vi.fn();
const revealTransferMock = vi.fn();
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
  /** 与生产一致：capabilities 含 transfer.resume.v1 才允许 resume UI */
  deviceSupportsTransferResume: (device?: { capabilities?: string[] } | null) =>
    Array.isArray(device?.capabilities) &&
    device.capabilities.includes('transfer.resume.v1'),
  TRANSFER_RESUME_CAPABILITY_V1: 'transfer.resume.v1',
}));

vi.mock('@/api/transfer', () => ({
  transferApi: {
    list: (...args: unknown[]) => listTransfersMock(...args),
    send: (...args: unknown[]) => sendTransferMock(...args),
    cancel: (...args: unknown[]) => cancelTransferMock(...args),
    retry: (...args: unknown[]) => retryTransferMock(...args),
    resume: (...args: unknown[]) => resumeTransferMock(...args),
    getOperation: (...args: unknown[]) => getOperationMock(...args),
    open: (...args: unknown[]) => openTransferMock(...args),
    reveal: (...args: unknown[]) => revealTransferMock(...args),
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
  capabilities: ['transfer.resume.v1', 'transfer.complete.v1'],
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
    peerDeviceId: 'device-a',
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

/**
 * Business Logic（为什么需要这个函数）:
 *   action matrix 断言需限制在某个任务行内。
 *
 * Code Logic（这个函数做什么）:
 *   通过文件名找到 li，再 within 查询按钮。
 */
function taskRowByFileName(fileName: string): HTMLElement {
  const nameNode = screen.getByText(fileName);
  const row = nameNode.closest('li');
  if (!row) throw new Error(`row not found for ${fileName}`);
  return row;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   验证任务行只出现期望动作。
 *
 * Code Logic（这个函数做什么）:
 *   within(row) 检查动作按钮集合。
 */
function expectRowActions(fileName: string, actions: string[]): void {
  const row = taskRowByFileName(fileName);
  const known = ['取消', '继续传输', '重新传输', '打开', '在文件夹中显示', '下载', '暂停', '重试'];
  for (const name of known) {
    const nodes = within(row).queryAllByRole('button', { name });
    if (actions.includes(name)) {
      expect(nodes.length).toBeGreaterThan(0);
    } else {
      expect(nodes.length).toBe(0);
    }
  }
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
  retryTransferMock.mockReset();
  resumeTransferMock.mockReset();
  getOperationMock.mockReset();
  getOperationMock.mockResolvedValue({ status: 'notFound', code: 'not_found' });
  openTransferMock.mockReset();
  revealTransferMock.mockReset();
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
      expect(sendTransferMock.mock.calls[0][0]).toBe(MOBILE_INBOX_DEVICE_ID);
      expect(sendTransferMock.mock.calls[0][1]).toBe(windowsPath);
      expect(typeof sendTransferMock.mock.calls[0][2]).toBe('string');
      const sendBtn = screen.getByRole('button', { name: /发送「report\.txt」/ });
      expect(sendBtn.hasAttribute('disabled')).toBe(true);
      expect(sendBtn.getAttribute('aria-busy')).toBe('true');
    });

    await act(async () => {
      resolveSend?.({
        accepted: true,
        deviceId: MOBILE_INBOX_DEVICE_ID,
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
    // 非 uncertain 错误文案（不含 timeout/network/offline），走 definitive failure 分支
    sendTransferMock.mockRejectedValueOnce(new Error('permission denied'));

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
      expect(screen.getByRole('alert').textContent).toContain('发送失败：permission denied');
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
      expect(sendTransferMock.mock.calls[0][0]).toBe(MOBILE_INBOX_DEVICE_ID);
      expect(sendTransferMock.mock.calls[0][1]).toBe(windowsPath);
      expect(typeof sendTransferMock.mock.calls[0][2]).toBe('string');
    });

    await act(async () => {
      resolveSend?.({
        accepted: true,
        deviceId: MOBILE_INBOX_DEVICE_ID,
        filePath: windowsPath,
        id: 'transfer-dbl',
      });
    });
  });

  test('device dropdown pins phone first and still sends with no LAN peers', async () => {
    listDevicesMock.mockResolvedValue([]);
    pickTransferFileMock.mockResolvedValueOnce('/tmp/solo.txt');
    sendTransferMock.mockResolvedValueOnce({
      accepted: true,
      deviceId: MOBILE_INBOX_DEVICE_ID,
      filePath: '/tmp/solo.txt',
      id: 'inbox-1',
    });

    renderTransfer();

    await waitFor(() => {
      const select = screen.getByRole('combobox', { name: '选择目标设备' }) as HTMLSelectElement;
      expect(within(select).getByRole('option', { name: '手机' })).toBeTruthy();
      expect(select.value).toBe(MOBILE_INBOX_DEVICE_ID);
    });

    fireEvent.click(screen.getByRole('button', { name: '浏览…' }));
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /发送「solo\.txt」/ }).hasAttribute('disabled')).toBe(
        false,
      );
    });
    fireEvent.click(screen.getByRole('button', { name: /发送「solo\.txt」/ }));
    await waitFor(() => {
      expect(sendTransferMock.mock.calls[0][0]).toBe(MOBILE_INBOX_DEVICE_ID);
    });
  });

  test('selecting a LAN peer still sends to that device', async () => {
    pickTransferFileMock.mockResolvedValueOnce('/tmp/lan.txt');
    sendTransferMock.mockResolvedValueOnce({
      accepted: true,
      deviceId: 'device-a',
      filePath: '/tmp/lan.txt',
      id: 'lan-1',
    });

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByRole('option', { name: /MacBook Pro/ })).toBeTruthy();
    });
    fireEvent.change(screen.getByRole('combobox', { name: '选择目标设备' }), {
      target: { value: 'device-a' },
    });
    fireEvent.click(screen.getByRole('button', { name: '浏览…' }));
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /发送「lan\.txt」/ }).hasAttribute('disabled')).toBe(
        false,
      );
    });
    fireEvent.click(screen.getByRole('button', { name: /发送「lan\.txt」/ }));
    await waitFor(() => {
      expect(sendTransferMock.mock.calls[0][0]).toBe('device-a');
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

  test('refresh failure preserves existing task list on network offline', async () => {
    listTransfersMock.mockResolvedValue([
      buildTask({ id: 'keep-1', fileName: 'keep-alive.txt', status: 'completed', progress: 1 }),
    ]);

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByText('keep-alive.txt')).toBeTruthy();
    });

    // networkOffline → keepStale；错误文案优先展示稳定 code
    listTransfersMock.mockRejectedValueOnce(new Error('network offline'));

    await act(async () => {
      await getPolling(3000).task();
    });

    expect(screen.getByText('keep-alive.txt')).toBeTruthy();
    expect(screen.getByRole('status').textContent).toContain('任务列表加载失败：NETWORK_OFFLINE');
  });

  test('refresh failure clears task list on malformed payload', async () => {
    listTransfersMock.mockResolvedValue([
      buildTask({ id: 'clear-1', fileName: 'to-clear.txt', status: 'completed', progress: 1 }),
    ]);

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByText('to-clear.txt')).toBeTruthy();
    });

    const syntaxErr = new SyntaxError('Unexpected token');
    listTransfersMock.mockRejectedValueOnce(syntaxErr);

    await act(async () => {
      await getPolling(3000).task();
    });

    expect(screen.queryByText('to-clear.txt')).toBeNull();
    expect(screen.getByRole('status').textContent).toContain('任务列表加载失败：MALFORMED_JSON');
  });

  test.each([
    [
      'transferring',
      buildTask({
        id: 't-transferring',
        status: 'transferring',
        fileName: 'a.bin',
      }),
      ['取消'],
    ],
    [
      'failed-resumable',
      buildTask({
        id: 't-resumable',
        status: 'failed',
        fileName: 'b.bin',
        progress: 0.4,
        transferredBytes: 800,
        failure: {
          stage: 'transfer',
          code: 'chunk_failed',
          retryable: true,
          message: 'drop',
        },
        speed: undefined,
      }),
      ['继续传输'],
    ],
    [
      'failed-retryable',
      buildTask({
        id: 't-retryable',
        status: 'failed',
        fileName: 'c.bin',
        progress: 0,
        transferredBytes: 0,
        failure: {
          stage: 'connect',
          code: 'connect_failed',
          retryable: true,
          message: 'offline',
        },
        speed: undefined,
      }),
      ['重新传输'],
    ],
    [
      'completed-received',
      buildTask({
        id: 't-received',
        status: 'completed',
        direction: 'receive',
        fileName: 'd.bin',
        progress: 1,
        speed: undefined,
      }),
      ['打开', '在文件夹中显示'],
    ],
  ] as const)('%s renders only legal actions', async (_fixture, task, actions) => {
    listTransfersMock.mockResolvedValue([task]);
    renderTransfer();

    await waitFor(() => {
      expect(screen.getByText(task.fileName)).toBeTruthy();
    });

    expectRowActions(task.fileName, [...actions]);
  });

  test('completed inbox offer does not render open/reveal/download', async () => {
    listTransfersMock.mockResolvedValue([
      buildTask({
        id: 't-inbox',
        status: 'completed',
        direction: 'send',
        fileName: 'phone.bin',
        progress: 1,
        peerDeviceId: MOBILE_INBOX_DEVICE_ID,
        peerDeviceName: undefined,
        speed: undefined,
      }),
    ]);

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByText('phone.bin')).toBeTruthy();
    });

    expectRowActions('phone.bin', []);
  });

  test('completed send does not render open/reveal', async () => {
    listTransfersMock.mockResolvedValue([
      buildTask({
        id: 't-sent',
        status: 'completed',
        direction: 'send',
        fileName: 'sent.bin',
        progress: 1,
        speed: undefined,
      }),
    ]);

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByText('sent.bin')).toBeTruthy();
    });

    expectRowActions('sent.bin', []);
  });

  test('resume calls API with stable clientOperationId', async () => {
    listTransfersMock.mockResolvedValue([
      buildTask({
        id: 'task-resume',
        status: 'failed',
        fileName: 'resume.bin',
        progress: 0.5,
        transferredBytes: 1024,
        failure: {
          stage: 'transfer',
          code: 'chunk_failed',
          retryable: true,
          message: 'drop',
        },
        speed: undefined,
      }),
    ]);
    resumeTransferMock.mockResolvedValueOnce(
      buildTask({ id: 'task-resume-2', status: 'pending', fileName: 'resume.bin' }),
    );

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByText('resume.bin')).toBeTruthy();
    });

    fireEvent.click(screen.getByRole('button', { name: '继续传输' }));

    await waitFor(() => {
      expect(resumeTransferMock).toHaveBeenCalledTimes(1);
      const [taskId, clientOperationId] = resumeTransferMock.mock.calls[0] as [string, string];
      expect(taskId).toBe('task-resume');
      expect(typeof clientOperationId).toBe('string');
      expect(clientOperationId.length).toBeGreaterThan(0);
    });
  });

  test('retry timeout enters reconciling and suppresses duplicate action', async () => {
    listTransfersMock.mockResolvedValue([
      buildTask({
        id: 'task-uncertain',
        status: 'failed',
        fileName: 'uncertain.bin',
        progress: 0,
        transferredBytes: 0,
        failure: {
          stage: 'connect',
          code: 'timeout',
          retryable: true,
          message: 'timeout',
        },
        speed: undefined,
      }),
    ]);
    const timeoutErr = Object.assign(new Error('request timeout'), { code: 'TIMEOUT' });
    retryTransferMock.mockRejectedValueOnce(timeoutErr);
    getOperationMock.mockResolvedValueOnce({ status: 'pending' });

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByText('uncertain.bin')).toBeTruthy();
    });

    fireEvent.click(screen.getByRole('button', { name: '重新传输' }));

    await waitFor(() => {
      expect(retryTransferMock).toHaveBeenCalledTimes(1);
      expect(getOperationMock).toHaveBeenCalledTimes(1);
      expect(screen.getAllByText('正在确认结果').length).toBeGreaterThan(0);
      expect(screen.queryByRole('button', { name: '重新传输' })).toBeNull();
    });
  });

  test('open and reveal invoke APIs for received completed', async () => {
    listTransfersMock.mockResolvedValue([
      buildTask({
        id: 'task-open',
        status: 'completed',
        direction: 'receive',
        fileName: 'open-me.bin',
        progress: 1,
        speed: undefined,
      }),
    ]);
    openTransferMock.mockResolvedValueOnce({
      taskId: 'task-open',
      action: 'open',
      path: '/tmp/open-me.bin',
    });
    revealTransferMock.mockResolvedValueOnce({
      taskId: 'task-open',
      action: 'reveal',
      path: '/tmp/open-me.bin',
    });

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByText('open-me.bin')).toBeTruthy();
    });

    fireEvent.click(screen.getByRole('button', { name: '打开' }));
    await waitFor(() => {
      expect(openTransferMock).toHaveBeenCalledWith('task-open');
    });

    fireEvent.click(screen.getByRole('button', { name: '在文件夹中显示' }));
    await waitFor(() => {
      expect(revealTransferMock).toHaveBeenCalledWith('task-open');
    });
  });

  test('groups tasks and omits empty sections', async () => {
    listTransfersMock.mockResolvedValue([
      buildTask({ id: 'a1', status: 'transferring', fileName: 'active.bin' }),
      buildTask({
        id: 'f1',
        status: 'failed',
        fileName: 'failed.bin',
        progress: 0,
        failure: {
          stage: 'connect',
          code: 'x',
          retryable: true,
          message: 'x',
        },
        speed: undefined,
      }),
    ]);

    renderTransfer();

    await waitFor(() => {
      expect(screen.getByText('active.bin')).toBeTruthy();
      expect(screen.getByText('failed.bin')).toBeTruthy();
    });

    expect(screen.getByText('进行中')).toBeTruthy();
    expect(screen.getByText('需要处理')).toBeTruthy();
    expect(screen.queryByText('最近完成')).toBeNull();
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
