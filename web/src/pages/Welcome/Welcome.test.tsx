// @vitest-environment jsdom
/**
 * Welcome 权限页单元测试（权限与 LAN gate 分离）。
 *
 * Business Logic（为什么需要这个测试）:
 *   Welcome skip 只写 skipped 标记；点「去设置」只 request，禁止自动 relaunch
 *   （避免从系统设置返回时闪白屏/反复重启）。
 *
 * Code Logic（这个测试做什么）:
 *   mock usePermissions + configApi；断言 skip → permissionSkippedKey；
 *   断言 go settings 调用 request 但不调用 relaunchForPermissions。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import {
  PERMISSION_ONBOARDED_KEY,
  permissionSkippedKey,
} from '@/hooks/usePermissions';

const requestMock = vi.fn(async () => undefined);
const refreshMock = vi.fn(async () => undefined);
const relaunchMock = vi.fn(async () => undefined);
const permissionsMock = vi.fn(async () => ({
  screenCapture: { granted: false },
  accessibility: { granted: false },
  inputMonitoring: { granted: false },
  notification: { granted: false },
}));

vi.mock('@/api/config', () => ({
  configApi: {
    appIdentity: vi.fn(async () => ({
      bundleId: 'com.cc-partner.app',
      flavor: 'release' as const,
    })),
    permissions: () => permissionsMock(),
    relaunchForPermissions: () => relaunchMock(),
  },
}));

vi.mock('@/hooks/usePermissions', async () => {
  const actual = await vi.importActual<typeof import('@/hooks/usePermissions')>(
    '@/hooks/usePermissions',
  );
  return {
    ...actual,
    usePermissions: () => ({
      status: {
        screenCapture: { granted: false },
        accessibility: { granted: false },
        inputMonitoring: { granted: false },
        notification: { granted: false },
      },
      loading: false,
      refreshing: false,
      error: null,
      requesting: new Set(),
      allRequiredGranted: false,
      allGranted: false,
      request: requestMock,
      requestMissing: vi.fn(),
      refresh: refreshMock,
    }),
  };
});

import { Welcome } from './Welcome';

describe('Welcome', () => {
  beforeEach(() => {
    localStorage.clear();
    requestMock.mockClear();
    refreshMock.mockClear();
    relaunchMock.mockClear();
    permissionsMock.mockClear();
    permissionsMock.mockResolvedValue({
      screenCapture: { granted: false },
      accessibility: { granted: false },
      inputMonitoring: { granted: false },
      notification: { granted: false },
    });
  });

  afterEach(() => {
    cleanup();
  });

  test('skip writes permission skipped marker only', async () => {
    await i18n.changeLanguage('zh');
    render(
      <I18nextProvider i18n={i18n}>
        <MemoryRouter>
          <Welcome />
        </MemoryRouter>
      </I18nextProvider>,
    );
    await waitFor(() => {
      expect(screen.getByRole('button', { name: '暂时跳过' })).toBeTruthy();
    });
    fireEvent.click(screen.getByRole('button', { name: '暂时跳过' }));
    expect(localStorage.getItem(permissionSkippedKey('release'))).toBe('1');
    expect(localStorage.getItem(PERMISSION_ONBOARDED_KEY)).toBeNull();
  });

  test('go settings does not relaunch', async () => {
    await i18n.changeLanguage('zh');
    render(
      <I18nextProvider i18n={i18n}>
        <MemoryRouter>
          <Welcome />
        </MemoryRouter>
      </I18nextProvider>,
    );
    const go = await screen.findAllByRole('button', { name: '去设置' });
    fireEvent.click(go[0]!);
    await waitFor(() => {
      expect(requestMock).toHaveBeenCalled();
    });
    // 给 microtask/timer 机会误触发 relaunch
    await act(async () => {
      await Promise.resolve();
    });
    expect(relaunchMock).not.toHaveBeenCalled();
  });
});
