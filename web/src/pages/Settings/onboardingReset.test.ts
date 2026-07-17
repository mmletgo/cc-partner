// @vitest-environment node
/**
 * onboarding reset 编排顺序测试
 *
 * Business Logic（为什么需要这个测试）:
 *   重置首次启动引导必须先清后端 bootstrap/stop，再清 localStorage，最后 exitGui；
 *   后端失败时不得 exit。
 *
 * Code Logic（这个测试做什么）:
 *   纯函数重现 confirm 编排（不挂载完整 Settings hook），mock 依赖断言顺序。
 */

import { beforeEach, describe, expect, test, vi } from 'vitest';

const resetOnboardingGates = vi.fn();
const exitGui = vi.fn();
const flushAll = vi.fn();

/**
 * Business Logic（为什么需要这个函数）:
 *   与 useSettingsController.confirmOnboardingReset 保持同一顺序合同，便于单测不依赖 React。
 *
 * Code Logic（这个函数做什么）:
 *   reset → removeItem → flushAll(best-effort) → exitGui；reset 失败抛错且不 exit。
 */
async function runOnboardingResetConfirm(deps: {
  resetOnboardingGates: () => Promise<unknown>;
  exitGui: () => Promise<void>;
  flushAll: () => Promise<void>;
  removeItem: (key: string) => void;
  permissionKey: string;
}): Promise<void> {
  await deps.resetOnboardingGates();
  try {
    deps.removeItem(deps.permissionKey);
  } catch {
    // ignore
  }
  try {
    await deps.flushAll();
  } catch {
    // ignore
  }
  await deps.exitGui();
}

describe('onboarding reset confirm sequence', () => {
  beforeEach(() => {
    resetOnboardingGates.mockReset();
    exitGui.mockReset();
    flushAll.mockReset();
    resetOnboardingGates.mockResolvedValue({
      ok: true,
      lanDisclosureReset: true,
      backendStopped: true,
    });
    exitGui.mockResolvedValue(undefined);
    flushAll.mockResolvedValue(undefined);
  });

  test('calls reset, removes onboarding key, flushes, then exitGui', async () => {
    const order: string[] = [];
    const removeItem = vi.fn((key: string) => {
      order.push(`remove:${key}`);
    });
    resetOnboardingGates.mockImplementation(async () => {
      order.push('reset');
      return { ok: true, lanDisclosureReset: true, backendStopped: true };
    });
    flushAll.mockImplementation(async () => {
      order.push('flush');
    });
    exitGui.mockImplementation(async () => {
      order.push('exit');
    });

    await runOnboardingResetConfirm({
      resetOnboardingGates,
      exitGui,
      flushAll,
      removeItem,
      permissionKey: 'cp-permission-onboarded',
    });

    expect(order).toEqual([
      'reset',
      'remove:cp-permission-onboarded',
      'flush',
      'exit',
    ]);
  });

  test('does not exit when reset fails', async () => {
    resetOnboardingGates.mockRejectedValue(new Error('boom'));
    const removeItem = vi.fn();

    await expect(
      runOnboardingResetConfirm({
        resetOnboardingGates,
        exitGui,
        flushAll,
        removeItem,
        permissionKey: 'cp-permission-onboarded',
      }),
    ).rejects.toThrow('boom');

    expect(removeItem).not.toHaveBeenCalled();
    expect(exitGui).not.toHaveBeenCalled();
  });

  test('flush failure still exits after successful reset', async () => {
    flushAll.mockRejectedValue(new Error('flush failed'));
    const removeItem = vi.fn();

    await runOnboardingResetConfirm({
      resetOnboardingGates,
      exitGui,
      flushAll,
      removeItem,
      permissionKey: 'cp-permission-onboarded',
    });

    expect(removeItem).toHaveBeenCalledWith('cp-permission-onboarded');
    expect(exitGui).toHaveBeenCalledTimes(1);
  });
});
