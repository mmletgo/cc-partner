/**
 * providerManagerApi deviceId 分流契约单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   Provider Manager 支持把 summary/switch 的作用目标切到局域网对端 cc-partner 设备；
 *   Tauri 命令契约是可选 `deviceId` 参数（Rust Option<String>）——本机调用不得携带该键，
 *   远端调用必须携带，避免后端把缺失键误判为远端。
 *
 * Code Logic（这个测试做什么）:
 *   mock ./client 的 invokeDecoded，断言命令名与 args 形状（deviceId 有无分流）。
 */

import { beforeEach, describe, expect, test, vi } from 'vitest';

const mockInvokeDecoded = vi.fn();

vi.mock('./client', () => ({
  invokeDecoded: (...args: unknown[]) => mockInvokeDecoded(...args),
}));

import { providerManagerApi } from './providerManager';

describe('providerManagerApi deviceId 分流', () => {
  beforeEach(() => {
    mockInvokeDecoded.mockReset();
    mockInvokeDecoded.mockResolvedValue({});
  });

  test('status() 本机：不携带 deviceId 参数', async () => {
    await providerManagerApi.status();
    expect(mockInvokeDecoded).toHaveBeenCalledWith(
      'provider_manager_status',
      undefined,
      expect.anything(),
    );
  });

  test('status(null) 本机：不携带 deviceId 参数', async () => {
    await providerManagerApi.status(null);
    expect(mockInvokeDecoded).toHaveBeenCalledWith(
      'provider_manager_status',
      undefined,
      expect.anything(),
    );
  });

  test('status(deviceId) 远端：args 携带 { deviceId }', async () => {
    await providerManagerApi.status('dev-9');
    expect(mockInvokeDecoded).toHaveBeenCalledWith(
      'provider_manager_status',
      { deviceId: 'dev-9' },
      expect.anything(),
    );
  });

  test('switch 本机：args 只有 app/providerId', async () => {
    await providerManagerApi.switch('claude', 'p1');
    expect(mockInvokeDecoded).toHaveBeenCalledWith(
      'provider_manager_switch',
      { app: 'claude', providerId: 'p1' },
      expect.anything(),
    );
  });

  test('switch 远端：args 追加 { deviceId }', async () => {
    await providerManagerApi.switch('codex', 'p2', 'dev-9');
    expect(mockInvokeDecoded).toHaveBeenCalledWith(
      'provider_manager_switch',
      { app: 'codex', providerId: 'p2', deviceId: 'dev-9' },
      expect.anything(),
    );
  });
});
