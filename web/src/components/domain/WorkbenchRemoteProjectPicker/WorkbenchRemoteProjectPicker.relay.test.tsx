/**
 * WorkbenchRemoteProjectPicker 影子设备（中转）渲染测试。
 *
 * Business Logic（为什么需要这个测试文件）:
 *   经跳板可见的影子设备必须与直连设备同列表渲染：中转 Pill 标记、
 *   离线影子置灰并提示原因、直连+影子同 id 并存时只渲染直连条目；
 *   这些行为直接决定跨网段用户能否正确选择远端设备。
 *
 * Code Logic（这个测试做什么）:
 *   mock devicesApi.list 与 workbenchApi.remote.*，渲染 source=remote 选择器；
 *   断言 Pill 文案、离线影子的 disabled 与提示文案、同 id 去重后的按钮数量。
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import type { Device } from '@/lib/types';

const devicesList = vi.fn();
const remoteRoots = vi.fn();
const remoteListDir = vi.fn();
const remoteInfo = vi.fn();
const remoteOpenProject = vi.fn();

vi.mock('@/api/devices', () => ({
  devicesApi: {
    list: (...args: unknown[]) => devicesList(...args),
  },
}));

vi.mock('@/api/workbench', async () => {
  const actual = await vi.importActual<typeof import('@/api/workbench')>('@/api/workbench');
  return {
    ...actual,
    workbenchApi: {
      ...actual.workbenchApi,
      remote: {
        ...actual.workbenchApi.remote,
        roots: (...args: unknown[]) => remoteRoots(...args),
        listDir: (...args: unknown[]) => remoteListDir(...args),
        info: (...args: unknown[]) => remoteInfo(...args),
        openProject: (...args: unknown[]) => remoteOpenProject(...args),
      },
    },
  };
});

import { WorkbenchRemoteProjectPicker } from './WorkbenchRemoteProjectPicker';

/**
 * Business Logic（为什么需要这个函数）:
 *   影子/直连设备 fixture 需要统一形状，避免各用例字面量漂移。
 *
 * Code Logic（这个函数做什么）:
 *   构造 Device（可选 via 中转标记与在线状态）。
 */
function makeDevice(overrides: Partial<Device> & Pick<Device, 'id' | 'name'>): Device {
  return {
    address: '10.0.0.9',
    port: 62116,
    status: 'online',
    ...overrides,
  };
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   设备列表渲染前需要 roots/info mock 就位，否则「打开项目」保持禁用影响交互。
 *
 * Code Logic（这个函数做什么）:
 *   填充单一 home root 与可读 pathInfo。
 */
function mockRemoteBrowse(): void {
  remoteRoots.mockResolvedValue([{ label: 'Home', path: '/home/demo', kind: 'home' }]);
  remoteListDir.mockResolvedValue([]);
  remoteInfo.mockResolvedValue({
    name: 'demo',
    path: '/home/demo',
    kind: 'dir',
    readable: true,
    isGitRepo: true,
    suggestedProjectName: 'demo',
  });
}

function renderPicker(): void {
  render(
    <I18nextProvider i18n={i18n}>
      <WorkbenchRemoteProjectPicker
        source="remote"
        onCancel={() => undefined}
        onProjectOpened={() => undefined}
      />
    </I18nextProvider>,
  );
}

describe('WorkbenchRemoteProjectPicker relay shadow devices', () => {
  test('renders shadow device with relay pill next to device name', async () => {
    devicesList.mockResolvedValue([
      makeDevice({ id: 'dev-b', name: 'nas-vpn' }),
      makeDevice({
        id: 'dev-c',
        name: 'power-vpn',
        viaDeviceId: 'dev-b',
        viaDeviceName: 'nas-vpn',
      }),
    ]);
    mockRemoteBrowse();
    renderPicker();

    const section = await screen.findByRole('region', { name: '在线设备' });
    // 设备列表在 region 出现后仍可能处于「加载中」，需异步等待按钮渲染
    const shadowButton = await within(section).findByRole('button', { name: /power-vpn/ });
    expect(within(shadowButton).getByText('经 nas-vpn 中转')).toBeDefined();
    // 直连设备不渲染中转标记；影子按钮的 Pill 文本也含跳板名，故锚定 name 开头精确取直连按钮
    const directButton = within(section).getByRole('button', { name: /^在线nas-vpn/ });
    expect(within(directButton).queryByText(/中转/)).toBeNull();
    expect(shadowButton.hasAttribute('disabled')).toBe(false);
  });

  test('greys out offline shadow device with relay unreachable hint', async () => {
    devicesList.mockResolvedValue([
      makeDevice({ id: 'dev-b', name: 'nas-vpn' }),
      makeDevice({
        id: 'dev-c',
        name: 'power-vpn',
        status: 'offline',
        viaDeviceId: 'dev-b',
        viaDeviceName: 'nas-vpn',
      }),
    ]);
    mockRemoteBrowse();
    renderPicker();

    const section = await screen.findByRole('region', { name: '在线设备' });
    const shadowButton = await within(section).findByRole('button', { name: /power-vpn/ });
    expect(shadowButton.hasAttribute('disabled')).toBe(true);
    expect(
      within(shadowButton).getByText('中转设备 nas-vpn 不可达或目标已下线'),
    ).toBeDefined();
  });

  test('renders only the direct entry when same id has both direct and shadow rows', async () => {
    devicesList.mockResolvedValue([
      makeDevice({ id: 'dev-c', name: 'power-vpn', viaDeviceId: 'dev-b', viaDeviceName: 'nas-vpn' }),
      makeDevice({ id: 'dev-c', name: 'power-vpn' }),
      makeDevice({ id: 'dev-b', name: 'nas-vpn' }),
    ]);
    mockRemoteBrowse();
    renderPicker();

    const section = await screen.findByRole('region', { name: '在线设备' });
    const powerButtons = await within(section).findAllByRole('button', { name: /power-vpn/ });
    expect(powerButtons).toHaveLength(1);
    expect(within(powerButtons[0] as HTMLElement).queryByText(/中转/)).toBeNull();
  });

  test('loads remote roots for the selected shadow device id', async () => {
    devicesList.mockResolvedValue([
      makeDevice({
        id: 'dev-c',
        name: 'power-vpn',
        viaDeviceId: 'dev-b',
        viaDeviceName: 'nas-vpn',
      }),
    ]);
    mockRemoteBrowse();
    const user = userEvent.setup();
    renderPicker();

    const section = await screen.findByRole('region', { name: '在线设备' });
    await user.click(await within(section).findByRole('button', { name: /power-vpn/ }));
    await waitFor(() => {
      expect(remoteRoots).toHaveBeenCalledWith('dev-c');
    });
  });
});
