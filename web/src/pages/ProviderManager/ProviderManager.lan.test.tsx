// @vitest-environment jsdom
/**
 * ProviderManager 局域网目标设备测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   Provider Manager 支持把 summary/switch 的作用目标切到局域网内其他 cc-partner 设备：
 *   选择器只列在线远端设备（不按 capabilities 过滤，mDNS 有 220 字节截断不可靠），
 *   选中远端后 status/switch 携带 deviceId，switching 在途时禁止切换设备避免竞态，
 *   远端模式下隐藏本机安装入口并展示远端版提示。
 *
 * Code Logic（这个测试做什么）:
 *   mock `@/api/providerManager` 与 `@/api/devices`：renderHook 验证 controller 分流
 *   （本机/远端、重载、online 过滤、switch 携带 deviceId、切换竞态守卫）；
 *   以手造 props 渲染 ProviderManagerView 验证选择器 chip 与远端 UI。
 */

import { act, cleanup, render, renderHook, screen, waitFor } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';

import i18n from '@/i18n';
import type { Device, ProviderManagerSummary } from '@/lib/types';

const statusMock = vi.fn();
const switchMock = vi.fn();
const installCliMock = vi.fn();
const listDevicesMock = vi.fn();

vi.mock('@/api/providerManager', () => ({
  providerManagerApi: {
    status: (...args: unknown[]) => statusMock(...args),
    list: vi.fn(),
    switch: (...args: unknown[]) => switchMock(...args),
    installCli: (...args: unknown[]) => installCliMock(...args),
  },
}));

vi.mock('@/api/devices', () => ({
  devicesApi: {
    list: (...args: unknown[]) => listDevicesMock(...args),
  },
}));

import { ProviderManagerView } from './ProviderManager';
import type { ProviderManagerViewProps } from './ProviderManager';
import { useProviderManagerController } from './useProviderManagerController';

/**
 * Business Logic（为什么需要这个函数）:
 *   异步 switch 测试需要手动 resolve，才能卡住 switching 在途窗口。
 *
 * Code Logic（这个函数做什么）:
 *   返回 promise 与 resolve/reject 控制器。
 */
function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void; reject: (reason?: unknown) => void } {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   多个用例共享最小合法 ProviderManagerSummary。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的 summary，默认 CLI 可用、claude 两个 provider。
 */
function buildSummary(overrides: Partial<ProviderManagerSummary> = {}): ProviderManagerSummary {
  return {
    ccSwitchDbPresent: true,
    cli: { available: true, path: '/usr/local/bin/cc-switch', version: '1.2.0' },
    gui: null,
    apps: [
      {
        app: 'claude',
        providers: [
          { id: 'p-deepseek', name: 'DeepSeek', category: null, isCurrent: true },
          { id: 'p-openrouter', name: 'OpenRouter', category: null, isCurrent: false },
        ],
        currentProviderId: 'p-deepseek',
      },
    ],
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   设备选择器用例共享最小合法 Device。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的在线设备（id/name/status 可定制）。
 */
function buildDevice(overrides: Partial<Device> = {}): Device {
  return {
    id: 'dev-1',
    name: 'Mac mini',
    address: '192.168.1.20',
    port: 62116,
    status: 'online',
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   view 是 pure 组件，直接以手造 props 渲染即可覆盖远端模式分支，无需 mock API。
 *
 * Code Logic（这个函数做什么）:
 *   构造完整 ProviderManagerViewProps，默认 CLI 缺失 + 两个在线设备，允许覆盖。
 */
function buildViewProps(overrides: Partial<ProviderManagerViewProps> = {}): ProviderManagerViewProps {
  return {
    summary: buildSummary({ cli: { available: false, path: null, version: null } }),
    loading: false,
    error: null,
    switchingKey: null,
    switchError: null,
    installing: false,
    installError: null,
    devices: [
      buildDevice(),
      buildDevice({
        id: 'dev-shadow',
        name: 'Shadow Box',
        viaDeviceId: 'dev-1',
        viaDeviceName: 'Mac mini',
      }),
    ],
    deviceId: null,
    isRemote: false,
    onSwitch: vi.fn(() => Promise.resolve()),
    onInstall: vi.fn(() => Promise.resolve()),
    onRecheck: vi.fn(() => Promise.resolve()),
    onSelectDevice: vi.fn(),
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   view 文案经 i18n 渲染，需统一挂在 I18nextProvider 下。
 *
 * Code Logic（这个函数做什么）:
 *   用 I18nextProvider 渲染 ProviderManagerView。
 */
function renderView(props: ProviderManagerViewProps): ReturnType<typeof render> {
  return render(
    <I18nextProvider i18n={i18n}>
      <ProviderManagerView {...props} />
    </I18nextProvider>,
  );
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

beforeEach(() => {
  statusMock.mockReset();
  switchMock.mockReset();
  installCliMock.mockReset();
  listDevicesMock.mockReset();
  statusMock.mockResolvedValue(buildSummary());
  listDevicesMock.mockResolvedValue([buildDevice()]);
});

afterEach(() => {
  cleanup();
});

describe('useProviderManagerController 目标设备分流', () => {
  test('首载读本机：status 不带远端 deviceId，设备列表只保留 online', async () => {
    listDevicesMock.mockResolvedValue([
      buildDevice(),
      buildDevice({ id: 'dev-2', name: 'Offline Box', status: 'offline' }),
    ]);
    const { result, unmount } = renderHook(() => useProviderManagerController());

    await waitFor(() => expect(result.current.loading).toBe(false));

    expect(statusMock).toHaveBeenCalledWith(null);
    expect(result.current.devices.map((device) => device.id)).toEqual(['dev-1']);
    expect(result.current.deviceId).toBeNull();
    expect(result.current.isRemote).toBe(false);
    unmount();
  });

  test('onSelectDevice 立即按远端 deviceId 重拉，重复选择同值不重复拉取', async () => {
    const { result, unmount } = renderHook(() => useProviderManagerController());
    await waitFor(() => expect(result.current.loading).toBe(false));

    statusMock.mockClear();
    act(() => {
      result.current.onSelectDevice('dev-1');
    });
    await waitFor(() => expect(statusMock).toHaveBeenLastCalledWith('dev-1'));
    expect(result.current.deviceId).toBe('dev-1');
    expect(result.current.isRemote).toBe(true);

    const callsAfterFirstSelect = statusMock.mock.calls.length;
    act(() => {
      result.current.onSelectDevice('dev-1');
    });
    expect(statusMock.mock.calls.length).toBe(callsAfterFirstSelect);
    unmount();
  });

  test('onSelectDevice(null) 切回本机并按本机重拉', async () => {
    const { result, unmount } = renderHook(() => useProviderManagerController());
    await waitFor(() => expect(result.current.loading).toBe(false));

    act(() => {
      result.current.onSelectDevice('dev-1');
    });
    await waitFor(() => expect(statusMock).toHaveBeenLastCalledWith('dev-1'));

    act(() => {
      result.current.onSelectDevice(null);
    });
    await waitFor(() => expect(statusMock).toHaveBeenLastCalledWith(null));
    expect(result.current.isRemote).toBe(false);
    unmount();
  });

  test('远端模式下 onSwitch 携带 deviceId', async () => {
    switchMock.mockResolvedValue(buildSummary().apps[0]);
    const { result, unmount } = renderHook(() => useProviderManagerController());
    await waitFor(() => expect(result.current.loading).toBe(false));

    act(() => {
      result.current.onSelectDevice('dev-1');
    });
    await waitFor(() => expect(statusMock).toHaveBeenLastCalledWith('dev-1'));

    await act(async () => {
      await result.current.onSwitch('claude', 'p-openrouter');
    });
    expect(switchMock).toHaveBeenCalledWith('claude', 'p-openrouter', 'dev-1');
    unmount();
  });

  test('switching 在途时 onSelectDevice 被忽略，避免切换竞态', async () => {
    const pending = deferred<ProviderManagerSummary['apps'][number]>();
    switchMock.mockReturnValue(pending.promise);
    const { result, unmount } = renderHook(() => useProviderManagerController());
    await waitFor(() => expect(result.current.loading).toBe(false));

    act(() => {
      result.current.onSelectDevice('dev-1');
    });
    await waitFor(() => expect(statusMock).toHaveBeenLastCalledWith('dev-1'));

    let switchPromise: Promise<void> = Promise.resolve();
    act(() => {
      switchPromise = result.current.onSwitch('claude', 'p-openrouter');
    });
    expect(result.current.switchingKey).toBe('claude:p-openrouter');

    const statusCallsDuringSwitch = statusMock.mock.calls.length;
    act(() => {
      result.current.onSelectDevice(null);
    });
    expect(result.current.deviceId).toBe('dev-1');
    expect(statusMock.mock.calls.length).toBe(statusCallsDuringSwitch);

    await act(async () => {
      pending.resolve(buildSummary().apps[0]);
      await switchPromise;
    });
    expect(result.current.switchingKey).toBeNull();
    unmount();
  });

  test('选中远端设备后 status 失败：保留选择，错误透出', async () => {
    const { result, unmount } = renderHook(() => useProviderManagerController());
    await waitFor(() => expect(result.current.loading).toBe(false));

    statusMock.mockRejectedValue(new Error('远端设备不在线'));
    act(() => {
      result.current.onSelectDevice('dev-1');
    });

    await waitFor(() => expect(result.current.error).toContain('远端设备不在线'));
    expect(result.current.deviceId).toBe('dev-1');
    expect(result.current.isRemote).toBe(true);
    unmount();
  });
});

describe('ProviderManagerView 目标设备选择器', () => {
  test('渲染本机 + 在线远端 chip（影子设备带中转后缀），本机默认选中', () => {
    renderView(buildViewProps());

    const group = screen.getByRole('group', { name: '目标设备' });
    expect(group).toBeTruthy();
    expect(screen.getByRole('button', { name: '本机' }).getAttribute('aria-pressed')).toBe('true');
    expect(screen.getByRole('button', { name: 'Mac mini' }).getAttribute('aria-pressed')).toBe('false');
    expect(screen.getByRole('button', { name: 'Shadow Box（经 Mac mini）' })).toBeTruthy();
  });

  test('点击远端 chip 回调 onSelectDevice(device.id)', () => {
    const onSelectDevice = vi.fn();
    renderView(buildViewProps({ onSelectDevice }));

    screen.getByRole('button', { name: 'Mac mini' }).click();
    expect(onSelectDevice).toHaveBeenCalledWith('dev-1');
  });

  test('选中远端后：aria-pressed 转移、header 显示目标设备名', () => {
    renderView(
      buildViewProps({
        devices: [buildDevice()],
        deviceId: 'dev-1',
        isRemote: true,
      }),
    );

    expect(screen.getByRole('button', { name: '本机' }).getAttribute('aria-pressed')).toBe('false');
    expect(screen.getByRole('button', { name: 'Mac mini' }).getAttribute('aria-pressed')).toBe('true');
    expect(screen.getByText(/正在管理远端设备「Mac mini」/)).toBeTruthy();
  });

  test('本机 CLI 缺失：显示安装按钮与本机文案', () => {
    renderView(buildViewProps({ isRemote: false, deviceId: null }));

    expect(screen.getByRole('button', { name: '安装 cc-switch CLI' })).toBeTruthy();
    expect(screen.getByText(/未安装 cc-switch CLI/)).toBeTruthy();
  });

  test('远端 CLI 缺失：隐藏安装按钮，warn 换远端文案（需在对端设备安装）', () => {
    renderView(buildViewProps({ deviceId: 'dev-1', isRemote: true }));

    expect(screen.queryByRole('button', { name: '安装 cc-switch CLI' })).toBeNull();
    expect(screen.getByText(/远端设备未安装 cc-switch CLI/)).toBeTruthy();
    expect(screen.getByText(/请在对端设备上安装 cc-switch CLI/)).toBeTruthy();
  });

  test('远端加载失败：错误文案使用远端版本', () => {
    renderView(
      buildViewProps({
        deviceId: 'dev-1',
        isRemote: true,
        error: '远端设备不在线',
        summary: null,
      }),
    );

    expect(screen.getByText(/加载远端设备 provider 状态失败：远端设备不在线/)).toBeTruthy();
  });
});
