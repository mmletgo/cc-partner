// @vitest-environment jsdom

/**
 * transferHttp 解码与下载文件名契约。
 *
 * Business Logic（为什么需要这个测试）:
 *   移动端任务 JSON 不得把主机 path 写入 UI；设备列表必须识别 isSelf。
 *
 * Code Logic（这个测试做什么）:
 *   直接调用 decode helpers；断言 path 被清空、isSelf 保留。
 */

import { afterEach, describe, expect, test, vi } from 'vitest';
import {
  decodeMobileTransferDevice,
  decodeMobileTransferDevices,
  decodeMobileTransferTask,
  decodeMobileTransferTasks,
  parseDownloadFileName,
  transferHttp,
} from './transferHttp';

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   解码测试需要一份最小合法移动端任务。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的任务字面量。
 */
function buildTaskPayload(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: 'task-1',
    fileName: 'notes.txt',
    fileSize: 12,
    direction: 'receive',
    status: 'completed',
    progress: 1,
    startedAt: '2026-08-18T00:00:00.000Z',
    ...overrides,
  };
}

describe('transferHttp decode', () => {
  test('decodes devices including this-computer isSelf', () => {
    const devices = decodeMobileTransferDevices([
      {
        id: 'self-1',
        name: 'Hans Mac',
        address: '127.0.0.1',
        port: 62116,
        isSelf: true,
        online: true,
      },
      {
        id: 'peer-1',
        name: 'Office PC',
        address: '192.168.1.8',
        port: 62116,
        isSelf: false,
        status: 'online',
        capabilities: ['transfer.resume.v1'],
      },
    ]);

    expect(devices).toHaveLength(2);
    expect(devices[0]?.isSelf).toBe(true);
    expect(devices[0]?.status).toBe('online');
    expect(devices[1]?.isSelf).toBe(false);
    expect(devices[1]?.capabilities).toEqual(['transfer.resume.v1']);
  });

  test('strips host filePath from decoded tasks', () => {
    const task = decodeMobileTransferTask(
      buildTaskPayload({
        filePath: '/Users/hans/Downloads/secret.bin',
        peerDeviceName: 'This computer',
      }),
    );

    expect(task.fileName).toBe('notes.txt');
    expect(task.filePath).toBe('');
    expect(JSON.stringify(task)).not.toContain('/Users/hans');
  });

  test('accepts tasks that omit filePath', () => {
    const tasks = decodeMobileTransferTasks([buildTaskPayload()]);
    expect(tasks[0]?.filePath).toBe('');
    expect(tasks[0]?.fileName).toBe('notes.txt');
  });

  test('parseDownloadFileName keeps basename only', () => {
    expect(
      parseDownloadFileName(
        'attachment; filename="/var/cc-partner/receive/report.pdf"',
        'fallback.bin',
      ),
    ).toBe('report.pdf');
    expect(parseDownloadFileName(null, 'notes.txt')).toBe('notes.txt');
  });

  test('listTasks uses GET /api/mobile/transfer/tasks and strips paths', async () => {
    const fetchMock = vi.fn(async () => {
      return {
        ok: true,
        json: async () => [
          buildTaskPayload({
            filePath: '/tmp/host-only.png',
          }),
        ],
      } as Response;
    });
    vi.stubGlobal('fetch', fetchMock);

    const tasks = await transferHttp.listTasks();
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/mobile/transfer/tasks',
      expect.objectContaining({ method: 'GET' }),
    );
    expect(tasks[0]?.filePath).toBe('');
    expect(JSON.stringify(tasks)).not.toContain('/tmp/host-only.png');
  });

  test('listDevices uses GET /api/mobile/devices', async () => {
    const fetchMock = vi.fn(async () => {
      return {
        ok: true,
        json: async () => [
          {
            id: 'self-1',
            name: 'Host',
            address: '10.0.0.2',
            port: 62116,
            isSelf: true,
            online: true,
          },
        ],
      } as Response;
    });
    vi.stubGlobal('fetch', fetchMock);

    const devices = await transferHttp.listDevices();
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/mobile/devices',
      expect.objectContaining({ method: 'GET' }),
    );
    expect(devices[0]?.isSelf).toBe(true);
  });

  test('completeUpload posts empty body and decodes path-stripped task DTO', async () => {
    const fetchMock = vi.fn(async () => {
      return {
        ok: true,
        json: async () =>
          buildTaskPayload({
            id: 'recv-1',
            fileName: 'hello.txt',
            fileSize: 5,
            direction: 'receive',
            status: 'completed',
            transferredBytes: 5,
            attempt: 1,
            logicalTransferId: 'recv-1',
            attemptId: 'recv-1',
            protocolTransferId: 'recv-1',
          }),
      } as Response;
    });
    vi.stubGlobal('fetch', fetchMock);

    const task = await transferHttp.completeUpload('staging-1');
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/mobile/transfer/upload/complete/staging-1',
      expect.objectContaining({
        method: 'POST',
        body: '{}',
      }),
    );
    expect(task.id).toBe('recv-1');
    expect(task.fileName).toBe('hello.txt');
    expect(task.filePath).toBe('');
    expect(task).not.toHaveProperty('accepted');
  });

  test('uploadChunk accepts extra success field and uses X-Chunk-Offset', async () => {
    const fetchMock = vi.fn(async () => {
      return {
        ok: true,
        json: async () => ({ success: true, receivedBytes: 12 }),
      } as Response;
    });
    vi.stubGlobal('fetch', fetchMock);

    const result = await transferHttp.uploadChunk('staging-1', 0, new Blob(['hello world!']));
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/mobile/transfer/upload/chunk/staging-1',
      expect.objectContaining({
        method: 'POST',
        headers: expect.objectContaining({
          'X-Chunk-Offset': '0',
          'Content-Type': 'application/octet-stream',
        }),
      }),
    );
    expect(result.receivedBytes).toBe(12);
  });
});
