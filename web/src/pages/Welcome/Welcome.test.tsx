// @vitest-environment jsdom
/**
 * Welcome 权限页单元测试（权限与 LAN gate 分离）。
 *
 * Business Logic（为什么需要这个测试）:
 *   Welcome skip 只写 skipped 标记；点「去设置」只 request，禁止自动 relaunch
 *   （避免从系统设置返回时闪白屏/反复重启）。前台同步耗尽仍 denied 时才出现
 *   「重新打开应用」，且仅该按钮可 relaunch。
 *
 * Code Logic（这个测试做什么）:
 *   mock usePermissions + configApi；断言 skip → permissionSkippedKey；
 *   断言 go settings + visibility/focus + SYNC_DELAYS 全程不 relaunch；
 *   断言 needs_reopen 后点击「重新打开应用」才 relaunch 一次。
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
import { SYNC_DELAYS_MS } from './welcomePermissionFlow';

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

const DENIED_PERMISSIONS = {
  screenCapture: { granted: false },
  accessibility: { granted: false },
  inputMonitoring: { granted: false },
  notification: { granted: false },
} as const;

/**
 * Business Logic（为什么需要这个函数）:
 *   模拟用户从系统设置回到应用：visibility 可见 + focus 兜底。
 *
 * Code Logic（这个函数做什么）:
 *   将 document.visibilityState 设为 visible，派发 visibilitychange 与 focus。
 */
function fireReturnFromSettings() {
  Object.defineProperty(document, 'visibilityState', {
    configurable: true,
    get: () => 'visible' as DocumentVisibilityState,
  });
  document.dispatchEvent(new Event('visibilitychange'));
  window.dispatchEvent(new Event('focus'));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   前台同步按 SYNC_DELAYS_MS 多轮 recheck，测试须推进全部 delay。
 *
 * Code Logic（这个函数做什么）:
 *   用 fake timers 依次 advance 每个 SYNC_DELAYS_MS，并 flush 微任务。
 */
async function advanceSyncDelays() {
  for (const delay of SYNC_DELAYS_MS) {
    await act(async () => {
      if (delay > 0) {
        await vi.advanceTimersByTimeAsync(delay);
      } else {
        await Promise.resolve();
        await Promise.resolve();
      }
    });
  }
  // 收尾再 flush 一轮，确保最后一轮 permissions/dispatch 完成
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe('Welcome', () => {
  beforeEach(() => {
    localStorage.clear();
    requestMock.mockClear();
    refreshMock.mockClear();
    relaunchMock.mockClear();
    permissionsMock.mockClear();
    permissionsMock.mockResolvedValue({ ...DENIED_PERMISSIONS });
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
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

  test('go settings + foreground sync never auto-relaunches; reopen button relaunches once', async () => {
    vi.useFakeTimers();
    await i18n.changeLanguage('zh');
    render(
      <I18nextProvider i18n={i18n}>
        <MemoryRouter>
          <Welcome />
        </MemoryRouter>
      </I18nextProvider>,
    );

    // appIdentity / 首屏渲染
    await act(async () => {
      await Promise.resolve();
    });

    const go = screen.getAllByRole('button', { name: '去设置' });
    fireEvent.click(go[0]!);

    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(requestMock).toHaveBeenCalled();
    expect(relaunchMock).not.toHaveBeenCalled();

    // 从系统设置返回：visibility + focus 触发多轮 SYNC，全程不得 relaunch
    await act(async () => {
      fireReturnFromSettings();
      await Promise.resolve();
      await Promise.resolve();
    });
    await advanceSyncDelays();

    expect(relaunchMock).not.toHaveBeenCalled();
    // permissions 保持全 false → needs_reopen
    expect(screen.getByRole('button', { name: '重新打开应用' })).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: '重新打开应用' }));
    await act(async () => {
      await Promise.resolve();
    });
    expect(relaunchMock).toHaveBeenCalledTimes(1);
  });
});
