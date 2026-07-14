/**
 * E2E-TRANSFER-001 — 文件传输关键桌面旅程（L1 browser mock）。
 *
 * Business Logic（为什么需要这个套件）:
 *   验证原生绝对路径选择 → 在线设备 → send_transfer → 进度/完成/取消，
 *   以及未实现 pause/retry/open 不可点、dropzone 键盘可达。
 *
 * Code Logic（这个套件做什么）:
 *   使用 backendHarness 注册 list_devices/list_transfers/send/cancel 与 plugin:dialog|open；
 *   中途重绑 list_transfers 模拟 progress→completed 与 cancelled。
 */

import { expect, test } from './fixtures';
import {
  installAppLocalStorage,
  registerAppShellCommands,
} from './support/appBootstrap';
import type { PlaywrightBackendHarness } from './support/backendHarness';

const PEER = {
  id: 'peer-1',
  name: 'Peer Mac',
  address: '192.168.1.20',
  port: 62116,
  online: true,
  lastSeen: '2026-07-14T00:00:00.000Z',
};

const ABSOLUTE_PATH = '/tmp/report.txt';

/**
 * Business Logic（为什么需要这个函数）:
 *   任务列表 DTO 必须满足 transfer runtime schema。
 *
 * Code Logic（这个函数做什么）:
 *   构造合法 TransferTask，可用 partial 覆盖 status/progress。
 */
function makeTask(
  partial: {
    id?: string;
    status?: 'pending' | 'transferring' | 'completed' | 'failed' | 'cancelled';
    progress?: number;
    completedAt?: string;
  } = {},
) {
  return {
    id: partial.id ?? 'task-1',
    fileName: 'report.txt',
    filePath: ABSOLUTE_PATH,
    fileSize: 1024,
    direction: 'send' as const,
    status: partial.status ?? 'transferring',
    progress: partial.progress ?? 0.25,
    peerDeviceId: PEER.id,
    peerDeviceName: PEER.name,
    speed: 1024,
    startedAt: '2026-07-14T00:01:00.000Z',
    completedAt: partial.completedAt,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Transfer 旅程在 AppShell 基线上注册设备/任务/dialog/send/cancel。
 *
 * Code Logic（这个函数做什么）:
 *   registerAppShell + transfer 相关 sticky 命令。
 */
function registerTransferCommands(harness: PlaywrightBackendHarness): void {
  registerAppShellCommands(harness);
  harness.command('list_devices', { kind: 'resolve', value: [PEER] });
  harness.command('list_transfers', { kind: 'resolve', value: [] });
  harness.command('plugin:dialog|open', { kind: 'resolve', value: ABSOLUTE_PATH });
  harness.command('send_transfer', {
    kind: 'resolve',
    value: {
      accepted: true,
      deviceId: PEER.id,
      filePath: ABSOLUTE_PATH,
      id: 'task-1',
    },
  });
  harness.command('cancel_transfer', {
    kind: 'resolve',
    value: { ok: true, id: 'task-1' },
  });
}

test.describe('E2E-TRANSFER-001 Transfer critical journey', () => {
  test('English critical L1 UI strings when cp-lang=en', async ({ page, backendHarness }) => {
    await installAppLocalStorage(page, { lang: 'en' });
    registerTransferCommands(backendHarness);

    await page.goto('/transfer');
    await expect(page.getByRole('heading', { name: 'File Transfer' })).toBeVisible({
      timeout: 15_000,
    });
    await expect(page.getByRole('heading', { name: /Transfer tasks/ })).toBeVisible();
    await expect(
      page.getByRole('button', { name: 'Drop a file here or click to select' }),
    ).toBeVisible();
    await expect(page.getByLabel('Select target device')).toBeVisible();
  });

  test('native path send → progress/completed → cancel; unsupported actions hidden; dropzone keyboard', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerTransferCommands(backendHarness);

    await page.goto('/transfer');
    await expect(page.getByRole('heading', { name: '文件传输' })).toBeVisible({
      timeout: 15_000,
    });
    await expect(page.getByLabel('选择目标设备')).toBeVisible();
    await expect(page.getByRole('option', { name: /Peer Mac/ })).toHaveCount(1);

    // dropzone Enter 激活原生选择
    const dropzone = page.getByRole('button', { name: '拖拽文件到此处或点击选择' });
    await dropzone.focus();
    await dropzone.press('Enter');
    await expect(page.getByText('已选择：report.txt')).toBeVisible({ timeout: 5_000 });

    // Space 再次激活选择（仍返回同一绝对路径）
    await dropzone.focus();
    await dropzone.press(' ');
    await expect(page.getByText('已选择：report.txt')).toBeVisible();

    // 发送前把 list 绑到 transferring，send 后 force poll 可见进度
    backendHarness.command('list_transfers', {
      kind: 'resolve',
      value: [makeTask({ status: 'transferring', progress: 0.4 })],
    });

    await page.getByRole('button', { name: /发送「report\.txt」/ }).click();
    await expect(page.getByText('已选择：report.txt')).toHaveCount(0, { timeout: 5_000 });
    await expect(page.getByRole('list').getByText('report.txt')).toBeVisible({
      timeout: 5_000,
    });
    await expect(page.getByText('传输中').first()).toBeVisible();

    const sendCalls = backendHarness
      .calls()
      .filter((call) => call.type === 'invoke' && call.command === 'send_transfer');
    expect(sendCalls).toHaveLength(1);
    expect(sendCalls[0]).toMatchObject({
      type: 'invoke',
      command: 'send_transfer',
      args: { deviceId: PEER.id, filePath: ABSOLUTE_PATH },
    });

    // 未实现 pause/retry/open：任务行仅 cancel（scope 到任务列表，避免侧栏/壳层同名按钮干扰）
    const taskList = page.getByRole('list').filter({ hasText: 'report.txt' });
    await expect(taskList.getByRole('button', { name: '取消' })).toHaveCount(1);
    await expect(taskList.getByRole('button', { name: '暂停' })).toHaveCount(0);
    await expect(taskList.getByRole('button', { name: '重试' })).toHaveCount(0);
    await expect(taskList.getByRole('button', { name: '打开' })).toHaveCount(0);

    // progress → completed（下一轮 list poll）
    backendHarness.command('list_transfers', {
      kind: 'resolve',
      value: [
        makeTask({
          status: 'completed',
          progress: 1,
          completedAt: '2026-07-14T00:02:00.000Z',
        }),
      ],
    });
    await expect(page.getByText('已完成').first()).toBeVisible({ timeout: 8_000 });

    // 再次发送一条 transferring 任务以验证 cancel 接线
    backendHarness.command('list_transfers', {
      kind: 'resolve',
      value: [makeTask({ id: 'task-2', status: 'transferring', progress: 0.1 })],
    });
    backendHarness.command('send_transfer', {
      kind: 'resolve',
      value: {
        accepted: true,
        deviceId: PEER.id,
        filePath: ABSOLUTE_PATH,
        id: 'task-2',
      },
    });
    backendHarness.command('plugin:dialog|open', {
      kind: 'resolve',
      value: ABSOLUTE_PATH,
    });
    await page.getByRole('button', { name: '浏览…' }).click();
    await expect(page.getByText('已选择：report.txt')).toBeVisible();
    await page.getByRole('button', { name: /发送「report\.txt」/ }).click();
    await expect(page.getByText('传输中').first()).toBeVisible({ timeout: 5_000 });

    backendHarness.command('cancel_transfer', {
      kind: 'resolve',
      value: { ok: true, id: 'task-2' },
    });
    backendHarness.command('list_transfers', {
      kind: 'resolve',
      value: [
        makeTask({
          id: 'task-2',
          status: 'cancelled',
          progress: 0,
          completedAt: '2026-07-14T00:03:00.000Z',
        }),
      ],
    });
    await page.getByRole('button', { name: '取消' }).click();
    await expect(page.getByText('已取消').first()).toBeVisible({ timeout: 5_000 });

    const cancelCalls = backendHarness
      .calls()
      .filter((call) => call.type === 'invoke' && call.command === 'cancel_transfer');
    expect(cancelCalls.length).toBeGreaterThanOrEqual(1);
    const lastCancel = cancelCalls[cancelCalls.length - 1];
    expect(lastCancel).toMatchObject({
      type: 'invoke',
      command: 'cancel_transfer',
      args: { taskId: 'task-2' },
    });
  });
});
