// @vitest-environment jsdom
/**
 * Welcome 权限页单元测试（权限与 LAN gate 分离）。
 *
 * Business Logic（为什么需要这个测试）:
 *   Welcome skip 只写 skipped 标记；点「去设置」只 request，禁止自动 relaunch
 *   （避免从系统设置返回时闪白屏/反复重启）。前台同步耗尽仍 denied 时才出现
 *   「重新打开应用」，且仅该按钮可 relaunch。方案 A：重新检查也能推进到 needs_reopen。
 *
 * Code Logic（这个测试做什么）:
 *   mock usePermissions + configApi；断言 skip → permissionSkippedKey；
 *   断言 go settings + 调度/visibility + SYNC_DELAYS 全程不 relaunch；
 *   断言 needs_reopen 后点击「重新打开应用」才 relaunch 一次；
 *   断言「重新检查」在 sticky denied 时也能露出「重新打开应用」。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import {
  PERMISSION_ONBOARDED_KEY,
  permissionSkippedKey,
} from '@/hooks/usePermissions';
import { POST_SETTINGS_SYNC_SCHEDULE_MS, SYNC_DELAYS_MS } from './welcomePermissionFlow';

const requestMock = vi.fn(async () => ({
  permission: 'inputMonitoring',
  operation: 'request' as const,
  before: 'notDetermined',
  after: 'denied',
}));
const openSettingsMock = vi.fn(async () => ({
  permission: 'inputMonitoring',
  operation: 'openSettings' as const,
  before: 'denied',
  after: 'denied',
}));
const refreshMock = vi.fn(async () => undefined);
const relaunchMock = vi.fn(async () => undefined);
const permissionsMock = vi.fn(async () => ({
  screenCapture: { granted: false },
  accessibility: { granted: false },
  inputMonitoring: { granted: false, state: 'denied' as const },
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

let inputMonitoringState: 'granted' | 'denied' | 'notDetermined' | 'unavailable' = 'denied';

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
        inputMonitoring: {
          granted: inputMonitoringState === 'granted',
          state: inputMonitoringState,
        },
        notification: { granted: false },
      },
      loading: false,
      refreshing: false,
      error: null,
      requesting: new Set(),
      allRequiredGranted: false,
      allGranted: false,
      request: requestMock,
      openSettings: openSettingsMock,
      refresh: refreshMock,
    }),
  };
});

import { Welcome } from './Welcome';

const DENIED_PERMISSIONS = {
  screenCapture: { granted: false },
  accessibility: { granted: false },
  inputMonitoring: { granted: false, state: 'denied' },
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

/**
 * Business Logic（为什么需要这个函数）:
 *   sticky 去设置后按 POST_SETTINGS_SYNC_SCHEDULE_MS 主动同步，须推进到 needs_reopen。
 *
 * Code Logic（这个函数做什么）:
 *   推进 schedule 首个 delay + 整轮 SYNC_DELAYS。
 */
async function advancePostSettingsSyncToNeedsReopen() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(POST_SETTINGS_SYNC_SCHEDULE_MS[0]!);
    await Promise.resolve();
    await Promise.resolve();
  });
  await advanceSyncDelays();
}

describe('Welcome', () => {
  beforeEach(() => {
    localStorage.clear();
    requestMock.mockClear();
    openSettingsMock.mockClear();
    refreshMock.mockClear();
    relaunchMock.mockClear();
    permissionsMock.mockClear();
    permissionsMock.mockResolvedValue({ ...DENIED_PERMISSIONS });
    inputMonitoringState = 'denied';
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

  test('input monitoring notDetermined requests without opening settings', async () => {
    inputMonitoringState = 'notDetermined';
    await i18n.changeLanguage('zh');
    render(
      <I18nextProvider i18n={i18n}>
        <MemoryRouter>
          <Welcome />
        </MemoryRouter>
      </I18nextProvider>,
    );

    const card = screen.getByText('输入监控').closest('[data-granted]');
    expect(card).not.toBeNull();
    fireEvent.click(within(card as HTMLElement).getByRole('button', { name: '请求授权' }));

    await act(async () => Promise.resolve());
    expect(requestMock).toHaveBeenCalledWith('inputMonitoring');
    expect(openSettingsMock).not.toHaveBeenCalled();
  });

  test('input monitoring denied opens settings without requesting', async () => {
    inputMonitoringState = 'denied';
    await i18n.changeLanguage('zh');
    render(
      <I18nextProvider i18n={i18n}>
        <MemoryRouter>
          <Welcome />
        </MemoryRouter>
      </I18nextProvider>,
    );

    const card = screen.getByText('输入监控').closest('[data-granted]');
    expect(card).not.toBeNull();
    fireEvent.click(
      within(card as HTMLElement).getByRole('button', { name: '打开系统设置' }),
    );

    await act(async () => Promise.resolve());
    expect(openSettingsMock).toHaveBeenCalledWith('inputMonitoring');
    expect(requestMock).not.toHaveBeenCalled();
  });

  test('input monitoring unavailable shows build help without permission IPC', async () => {
    inputMonitoringState = 'unavailable';
    await i18n.changeLanguage('zh');
    render(
      <I18nextProvider i18n={i18n}>
        <MemoryRouter>
          <Welcome />
        </MemoryRouter>
      </I18nextProvider>,
    );

    const card = screen.getByText('输入监控').closest('[data-granted]');
    expect(card).not.toBeNull();
    fireEvent.click(within(card as HTMLElement).getByRole('button', { name: '查看构建说明' }));

    expect(screen.getByRole('status').textContent).toContain('稳定的内部代码签名');
    expect(requestMock).not.toHaveBeenCalled();
    expect(openSettingsMock).not.toHaveBeenCalled();
  });

  test('go settings + scheduled sync never auto-relaunches; reopen button relaunches once', async () => {
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

    const inputCard = screen.getByText('输入监控').closest('[data-granted]');
    fireEvent.click(
      within(inputCard as HTMLElement).getByRole('button', { name: '打开系统设置' }),
    );

    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(openSettingsMock).toHaveBeenCalled();
    expect(relaunchMock).not.toHaveBeenCalled();

    // 不依赖 visibility：POST_SETTINGS 调度即可耗尽到 needs_reopen
    await advancePostSettingsSyncToNeedsReopen();

    expect(relaunchMock).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: '重新打开应用' })).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: '重新打开应用' }));
    await act(async () => {
      await Promise.resolve();
    });
    expect(relaunchMock).toHaveBeenCalledTimes(1);
  });

  test('recheck with sticky denied reaches needs_reopen without auto-relaunch', async () => {
    vi.useFakeTimers();
    await i18n.changeLanguage('zh');
    render(
      <I18nextProvider i18n={i18n}>
        <MemoryRouter>
          <Welcome />
        </MemoryRouter>
      </I18nextProvider>,
    );

    await act(async () => {
      await Promise.resolve();
    });

    fireEvent.click(screen.getByRole('button', { name: '重新检查' }));

    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    // USER_RECHECK 后进入 syncing，推进 SYNC_DELAYS
    await advanceSyncDelays();

    expect(relaunchMock).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: '重新打开应用' })).toBeTruthy();
  });

  test('visibility return from settings still never auto-relaunches', async () => {
    vi.useFakeTimers();
    await i18n.changeLanguage('zh');
    render(
      <I18nextProvider i18n={i18n}>
        <MemoryRouter>
          <Welcome />
        </MemoryRouter>
      </I18nextProvider>,
    );

    await act(async () => {
      await Promise.resolve();
    });

    const inputCard = screen.getByText('输入监控').closest('[data-granted]');
    fireEvent.click(
      within(inputCard as HTMLElement).getByRole('button', { name: '打开系统设置' }),
    );
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    await act(async () => {
      fireReturnFromSettings();
      await Promise.resolve();
      await Promise.resolve();
    });
    await advanceSyncDelays();

    expect(relaunchMock).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: '重新打开应用' })).toBeTruthy();
  });
});
