// @vitest-environment jsdom
/**
 * useSettingsFormSaves characterization 测试
 *
 * Business Logic（为什么需要这个测试文件）:
 *   safe-save 合同从巨型 controller 拆出后，必须锁定：成功更新 baseline、
 *   并发编辑不回填草稿、失败只写 saveError 不写 loadError。
 *
 * Code Logic（这个测试文件做什么）:
 *   mock configApi.update；renderHook 断言 general save 的 resolveSave* 行为。
 */
import type { KeyboardEvent } from 'react';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';

const updateMock = vi.fn();
const chooseDirMock = vi.fn();
const listJobsMock = vi.fn();

vi.mock('@/api/config', () => ({
  configApi: {
    update: (...args: unknown[]) => updateMock(...args),
    chooseDir: (...args: unknown[]) => chooseDirMock(...args),
    updateCloudSyncConfig: vi.fn(),
    testCloudSync: vi.fn(),
    triggerCloudSync: vi.fn(),
  },
}));

vi.mock('@/api/health', () => ({
  healthApi: { updateConfig: vi.fn() },
}));

vi.mock('@/api/githubTrending', () => ({
  githubTrendingApi: { updateConfig: vi.fn(), testClaudeCli: vi.fn() },
}));

vi.mock('@/api/orchestratorConfig', () => ({
  orchestratorConfigApi: { update: vi.fn() },
}));

vi.mock('@/api/sync', () => ({
  syncApi: { trigger: vi.fn() },
  backupApi: {
    listJobs: (...args: unknown[]) => listJobsMock(...args),
    create: vi.fn(),
    inspect: vi.fn(),
    restore: vi.fn(),
    rollback: vi.fn(),
  },
  pickBackupExportPath: vi.fn(),
  pickBackupArchivePath: vi.fn(),
  BACKUP_RESTORE_DOMAINS: ['prompts', 'scratchpad', 'claudeMd'],
}));

// 稳定 t 身份：避免 backup jobs effect 因 t 每 render 新建而死循环
const { tFn } = vi.hoisted(() => ({
  tFn: (key: string) => key,
}));
vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: tFn,
    i18n: { language: 'zh' },
    ready: true,
  }),
}));

import { useSettingsFormSaves } from './useSettingsFormSaves';
import type { SettingsResourceResults } from '../settingsResources';

function buildCoreConfig(deviceName: string) {
  return {
    deviceId: 'd1',
    deviceName,
    receiveDir: '/tmp/in',
    gamePluginDir: '/tmp/plugins',
    screenshotHotkey: 'CommandOrControl+Shift+S',
    promptOptimizerHotkey: 'Control',
    promptOptimizerFillLanguage: 'zh' as const,
  };
}

function buildReadyResults(deviceName = 'Loaded'): SettingsResourceResults {
  const config = buildCoreConfig(deviceName);
  return {
    core: { status: 'ready', value: config as never },
    defaults: { status: 'ready', value: buildCoreConfig('Default') as never },
    version: { status: 'ready', value: { version: '1.0.0', tauriVersion: '2' } as never },
    cloudSync: {
      current: { status: 'ready', value: { enabled: false } as never },
      defaults: { status: 'ready', value: { enabled: false } as never },
    },
    githubTrending: {
      current: {
        status: 'ready',
        value: {
          aiEnabled: true,
          claudeCliPath: 'claude',
          claudeModel: 'sonnet',
          cacheTtlHours: 24,
        } as never,
      },
      defaults: {
        status: 'ready',
        value: {
          aiEnabled: true,
          claudeCliPath: 'claude',
          claudeModel: 'sonnet',
          cacheTtlHours: 24,
        } as never,
      },
    },
    health: {
      current: { status: 'ready', value: { enabled: true } as never },
      defaults: { status: 'ready', value: { enabled: true } as never },
    },
    automation: {
      current: {
        status: 'ready',
        value: {
          enabled: true,
          maxConcurrentTasks: 1,
          verificationCommands: [],
          autoCommit: false,
          autoPushTaskBranch: false,
          autoMergeToMain: false,
          autoPushMain: false,
          notifyHumanReview: true,
          notifyBlocked: true,
          notifyRemoteOutboxFailed: true,
          notifyTaskDone: false,
        } as never,
      },
      defaults: {
        status: 'ready',
        value: {
          enabled: true,
          maxConcurrentTasks: 1,
          verificationCommands: [],
          autoCommit: false,
          autoPushTaskBranch: false,
          autoMergeToMain: false,
          autoPushMain: false,
          notifyHumanReview: true,
          notifyBlocked: true,
          notifyRemoteOutboxFailed: true,
          notifyTaskDone: false,
        } as never,
      },
    },
  };
}

describe('useSettingsFormSaves', () => {
  beforeEach(() => {
    updateMock.mockReset();
    chooseDirMock.mockReset();
    listJobsMock.mockReset();
    listJobsMock.mockResolvedValue([]);
  });

  afterEach(() => {
    cleanup();
  });

  test('save 成功更新 baseline，草稿与 baseline 对齐', async () => {
    const { result } = renderHook(() => useSettingsFormSaves());
    await act(async () => {
      result.current.applyResourceResults(buildReadyResults('Loaded'));
    });

    act(() => {
      result.current.handleDeviceNameChange({
        target: { value: 'Renamed' },
      } as never);
    });
    expect(result.current.isDirty).toBe(true);

    updateMock.mockResolvedValue(buildCoreConfig('Renamed'));

    await act(async () => {
      await result.current.handleSave();
    });

    expect(result.current.saveError).toBeNull();
    expect(result.current.isDirty).toBe(false);
    expect(result.current.state.deviceName).toBe('Renamed');
    expect(result.current.savedAt).toBeInstanceOf(Date);
  });

  test('保存期间继续编辑时 success 不覆盖新草稿', async () => {
    const { result } = renderHook(() => useSettingsFormSaves());
    await act(async () => {
      result.current.applyResourceResults(buildReadyResults('Loaded'));
    });

    act(() => {
      result.current.handleDeviceNameChange({
        target: { value: 'First' },
      } as never);
    });

    let resolveUpdate: ((value: unknown) => void) | null = null;
    updateMock.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveUpdate = resolve;
        }),
    );

    let savePromise: Promise<void> | undefined;
    act(() => {
      savePromise = result.current.handleSave();
    });

    act(() => {
      result.current.handleDeviceNameChange({
        target: { value: 'Second' },
      } as never);
    });

    await act(async () => {
      resolveUpdate?.(buildCoreConfig('First'));
      await savePromise;
    });

    expect(result.current.state.deviceName).toBe('Second');
    expect(result.current.isDirty).toBe(true);
    expect(result.current.saveError).toBeNull();
  });

  test('save 失败 scoped 到 saveError，不抛、不污染 draft', async () => {
    const { result } = renderHook(() => useSettingsFormSaves());
    await act(async () => {
      result.current.applyResourceResults(buildReadyResults('Loaded'));
    });

    act(() => {
      result.current.handleDeviceNameChange({
        target: { value: 'Dirty' },
      } as never);
    });

    updateMock.mockRejectedValue(new Error('persist failed'));

    await act(async () => {
      await result.current.handleSave();
    });

    expect(result.current.saveError).toBe('persist failed');
    expect(result.current.state.deviceName).toBe('Dirty');
    expect(result.current.isDirty).toBe(true);
    // form saves 不暴露 loadError 字段——失败不得冒充加载错误
    expect('loadError' in result.current).toBe(false);
  });

  test('Cmd+Ctrl 录制被拒绝且保持录制态', async () => {
    const { result } = renderHook(() => useSettingsFormSaves());
    await act(async () => {
      result.current.applyResourceResults(buildReadyResults('Loaded'));
    });

    const blur = vi.fn();
    const input = {
      preventDefault() {},
      stopPropagation() {},
      currentTarget: { blur },
      key: 's',
      metaKey: true,
      ctrlKey: true,
      altKey: false,
      shiftKey: false,
    } as unknown as KeyboardEvent<HTMLInputElement>;

    act(() => {
      result.current.handleShortcutFocus('screenshot');
    });
    expect(result.current.recordingShortcutId).toBe('screenshot');

    const before = result.current.state.shortcuts.find((s) => s.id === 'screenshot')?.value;
    act(() => {
      result.current.handleShortcutKeyDown(input, 'screenshot');
    });

    expect(result.current.shortcutRecordingRejectReason).toBe('cmdCtrlConflict');
    expect(result.current.recordingShortcutId).toBe('screenshot');
    expect(result.current.state.shortcuts.find((s) => s.id === 'screenshot')?.value).toBe(before);
    expect(blur).not.toHaveBeenCalled();
  });
});
