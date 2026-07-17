// @vitest-environment node
/**
 * backendApi 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   LAN disclosure 命令必须走固定 invoke 名，避免 gate 与后端漂移。
 *
 * Code Logic（这个测试做什么）:
 *   mock invoke，断言 status 与 disclosure 命令名。
 */

import { beforeEach, describe, expect, test, vi } from 'vitest';

const invokeMock = vi.fn();

vi.mock('./client', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

import { backendApi } from './backend';

describe('backendApi', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue({});
  });

  test('getLanDisclosureStatus invokes get_lan_disclosure_status', async () => {
    await backendApi.getLanDisclosureStatus();
    expect(invokeMock).toHaveBeenCalledWith('get_lan_disclosure_status');
  });

  test('acknowledgeLanDisclosureAndStartBackend invokes acknowledge command', async () => {
    await backendApi.acknowledgeLanDisclosureAndStartBackend();
    expect(invokeMock).toHaveBeenCalledWith('acknowledge_lan_disclosure_and_start_backend');
  });

  test('resetOnboardingGates invokes reset_onboarding_gates', async () => {
    await backendApi.resetOnboardingGates();
    expect(invokeMock).toHaveBeenCalledWith('reset_onboarding_gates');
  });

  test('legacy lifecycle commands remain', async () => {
    await backendApi.status();
    await backendApi.start();
    await backendApi.stop();
    await backendApi.exitGui();
    expect(invokeMock).toHaveBeenCalledWith('get_backend_status');
    expect(invokeMock).toHaveBeenCalledWith('start_backend_process');
    expect(invokeMock).toHaveBeenCalledWith('stop_backend_process');
    expect(invokeMock).toHaveBeenCalledWith('exit_gui');
  });
});
