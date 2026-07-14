/**
 * syncApi / backupApi 契约单元测试
 *
 * Business Logic（为什么需要这个测试）:
 *   trigger_sync 必须返回 SyncRunResult；partial/unreachable 不得被 helper 判为成功；
 *   备份 create/inspect/restore/listJobs/rollback 必须按 camelCase 参数调 invoke。
 *
 * Code Logic（这个测试做什么）:
 *   mock invoke，断言命令名、参数与 isDeviceSucceeded/isDomainSucceeded 语义。
 */

import { beforeEach, describe, expect, test, vi } from 'vitest';
import type { BackupInspectPreview, BackupRestoreResult, SyncRunResult } from './sync';

const mockInvoke = vi.fn();

vi.mock('./client', () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

import {
  backupApi,
  isDeviceSucceeded,
  isDomainSucceeded,
  succeededCounts,
  syncApi,
} from './sync';

describe('syncApi', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  test('trigger invokes trigger_sync and returns SyncRunResult', async () => {
    const payload: SyncRunResult = {
      accepted: true,
      succeeded_devices: 0,
      synced: 0,
      note: '已与 0/1 个设备完全同步',
      devices: [
        {
          device_id: 'd1',
          device_name: 'Peer',
          status: 'partial',
          domains: [
            {
              domain: 'prompt',
              outcome: { kind: 'succeeded', pulled: 1, pushed: 0, unchanged: 2 },
            },
            {
              domain: 'ssh_target',
              outcome: { kind: 'succeeded', pulled: 0, pushed: 0, unchanged: 0 },
            },
            {
              domain: 'scratchpad',
              outcome: { kind: 'unreachable', class: 'network' },
            },
          ],
        },
      ],
    };
    mockInvoke.mockResolvedValueOnce(payload);

    const result = await syncApi.trigger();

    expect(mockInvoke).toHaveBeenCalledWith('trigger_sync');
    expect(result.succeeded_devices).toBe(0);
    expect(result.devices[0].status).toBe('partial');
  });

  test('partial and unreachable never count as success', () => {
    expect(isDeviceSucceeded('partial')).toBe(false);
    expect(isDeviceSucceeded('unreachable')).toBe(false);
    expect(isDeviceSucceeded('protocol_error')).toBe(false);
    expect(isDeviceSucceeded('resource_limit')).toBe(false);
    expect(isDeviceSucceeded('succeeded')).toBe(true);

    expect(isDomainSucceeded({ kind: 'unreachable', class: 'timeout' })).toBe(false);
    expect(
      isDomainSucceeded({ kind: 'succeeded', pulled: 0, pushed: 0, unchanged: 1 }),
    ).toBe(true);
    expect(
      succeededCounts({ kind: 'protocol_error', code: 'x' }),
    ).toBeNull();
    expect(
      succeededCounts({ kind: 'succeeded', pulled: 2, pushed: 1, unchanged: 3 }),
    ).toEqual({ pulled: 2, pushed: 1, unchanged: 3 });
  });
});

describe('backupApi', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  test('create invokes create_backup with destPath', async () => {
    mockInvoke.mockResolvedValueOnce({ path: '/tmp/out.zip', formatVersion: 1 });
    const result = await backupApi.create('/tmp/out.zip');
    expect(mockInvoke).toHaveBeenCalledWith('create_backup', { destPath: '/tmp/out.zip' });
    expect(result.path).toBe('/tmp/out.zip');
    expect(result.formatVersion).toBe(1);
  });

  test('inspect invokes inspect_backup with archivePath', async () => {
    const preview: BackupInspectPreview = {
      formatVersion: 1,
      domainCounts: { prompts: 2 },
      warnings: ['w1'],
      conflictsEstimate: 0,
    };
    mockInvoke.mockResolvedValueOnce(preview);
    const result = await backupApi.inspect('/tmp/in.zip');
    expect(mockInvoke).toHaveBeenCalledWith('inspect_backup', {
      archivePath: '/tmp/in.zip',
    });
    expect(result.domainCounts.prompts).toBe(2);
    expect(result.warnings).toEqual(['w1']);
  });

  test('restore invokes restore_backup with mode and domains', async () => {
    const payload: BackupRestoreResult = {
      jobId: 'job-1',
      status: 'succeeded',
      appliedDomains: ['prompts'],
      preRestoreBackupPath: '/tmp/pre.zip',
      errorSummary: null,
    };
    mockInvoke.mockResolvedValueOnce(payload);
    const result = await backupApi.restore('/tmp/in.zip', 'merge', ['prompts']);
    expect(mockInvoke).toHaveBeenCalledWith('restore_backup', {
      archivePath: '/tmp/in.zip',
      mode: 'merge',
      domains: ['prompts'],
    });
    expect(result.jobId).toBe('job-1');
    expect(result.appliedDomains).toEqual(['prompts']);
  });

  test('listJobs invokes list_recovery_jobs with optional limit', async () => {
    mockInvoke.mockResolvedValueOnce([]);
    await backupApi.listJobs(10);
    expect(mockInvoke).toHaveBeenCalledWith('list_recovery_jobs', { limit: 10 });
  });

  test('rollback invokes rollback_recovery_job with jobId', async () => {
    const payload: BackupRestoreResult = {
      jobId: 'job-2',
      status: 'succeeded',
      appliedDomains: ['prompts'],
    };
    mockInvoke.mockResolvedValueOnce(payload);
    const result = await backupApi.rollback('job-2');
    expect(mockInvoke).toHaveBeenCalledWith('rollback_recovery_job', { jobId: 'job-2' });
    expect(result.jobId).toBe('job-2');
  });
});
