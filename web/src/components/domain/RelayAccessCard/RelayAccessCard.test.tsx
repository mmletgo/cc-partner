/**
 * RelayAccessCard pure view 交互测试。
 *
 * Business Logic（为什么需要这个测试文件）:
 *   中转访问卡片承担跳板添加/移除、本机角色开关与影子清单展开四类用户动作，
 *   且约定数据与动作全部经 props 注入（不 import @/api）——必须锁住这些交互契约。
 *
 * Code Logic（这个测试做什么）:
 *   jsdom 渲染 + mock props：选择候选后添加、移除跳板、切换开关（aria-checked 翻转）、
 *   展开行显示影子清单、保存成功/失败 StatusMessage 反馈与加载失败提示。
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import type { RelayViaRow } from '@/lib/relayDevices';
import { RelayAccessCard } from './RelayAccessCard';

/**
 * Business Logic（为什么需要这个函数）:
 *   各用例需要形状一致的跳板行/候选 fixture，避免字面量漂移。
 *
 * Code Logic（这个函数做什么）:
 *   构造带影子清单的 RelayViaRow。
 */
function makeViaRow(overrides: Partial<RelayViaRow> & Pick<RelayViaRow, 'deviceId'>): RelayViaRow {
  return {
    deviceName: 'nas-vpn',
    address: '10.0.0.2',
    status: 'online',
    shadowCount: 0,
    shadows: [],
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   卡片必须包裹真实 i18n Provider 才能断言中文文案与 aria 属性。
 *
 * Code Logic（这个函数做什么）:
 *   以 zh 语言渲染 RelayAccessCard 并返回渲染容器。
 */
function renderCard(overrides: Partial<Parameters<typeof RelayAccessCard>[0]> = {}) {
  const props = {
    candidates: [
      { id: 'dev-b', name: 'nas-vpn', address: '10.0.0.2' },
      { id: 'dev-c', name: 'mac-mini', address: '10.0.0.3' },
    ],
    viaDevices: [] as RelayViaRow[],
    allowEnabled: true,
    loading: false,
    saving: false,
    loadError: null,
    saveError: null,
    saveSuccess: null,
    onAddViaDevice: vi.fn(),
    onRemoveViaDevice: vi.fn(),
    onToggleAllow: vi.fn(),
    onRefresh: vi.fn(),
    ...overrides,
  };
  const view = render(
    <I18nextProvider i18n={i18n}>
      <RelayAccessCard {...props} />
    </I18nextProvider>,
  );
  return { ...view, props };
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('RelayAccessCard', () => {
  test('renders title, risk notice and empty jump list', () => {
    renderCard();
    expect(screen.getByRole('heading', { name: '中转访问（跳板）' })).toBeDefined();
    expect(screen.getByText(/明文中转/)).toBeDefined();
    expect(screen.getByText('还没有添加跳板设备')).toBeDefined();
  });

  test('adds a jump device from the candidate selector', async () => {
    const user = userEvent.setup();
    const { props } = renderCard();
    const select = screen.getByRole('combobox', { name: '添加跳板设备' });
    await user.selectOptions(select, 'dev-c');
    await user.click(screen.getByRole('button', { name: '添加' }));
    expect(props.onAddViaDevice).toHaveBeenCalledWith('dev-c');
  });

  test('disable add button until a candidate is selected', () => {
    renderCard();
    expect(screen.getByRole('button', { name: '添加' }).hasAttribute('disabled')).toBe(true);
  });

  test('renders via rows with shadow count and remove action', () => {
    renderCard({
      viaDevices: [
        makeViaRow({
          deviceId: 'dev-b',
          shadowCount: 2,
          shadows: [
            { id: 'c-1', name: 'power-vpn', status: 'online' },
            { id: 'c-2', name: 'old-box', status: 'offline' },
          ],
        }),
      ],
    });
    expect(screen.getByText('nas-vpn')).toBeDefined();
    expect(screen.getByText('可见 2 台设备')).toBeDefined();
    expect(
      screen.getByRole('button', { name: '移除跳板 nas-vpn' }),
    ).toBeDefined();
  });

  test('expands a via row to show shadow devices', async () => {
    const user = userEvent.setup();
    renderCard({
      viaDevices: [
        makeViaRow({
          deviceId: 'dev-b',
          shadowCount: 1,
          shadows: [{ id: 'c-1', name: 'power-vpn', status: 'online' }],
        }),
      ],
    });
    // 展开摘要按钮与「移除跳板」按钮的 accessible name 都含设备名，用 aria-expanded 锁定 disclosure 按钮
    const summary = screen
      .getAllByRole('button', { name: /nas-vpn/ })
      .find((el) => el.hasAttribute('aria-expanded'))!;
    expect(summary.getAttribute('aria-expanded')).toBe('false');
    await user.click(summary);
    expect(summary.getAttribute('aria-expanded')).toBe('true');
    expect(screen.getByText('power-vpn')).toBeDefined();
    expect(screen.getByText('在线')).toBeDefined();
  });

  test('removes a via device via row action', async () => {
    const user = userEvent.setup();
    const { props } = renderCard({
      viaDevices: [makeViaRow({ deviceId: 'dev-b' })],
    });
    await user.click(screen.getByRole('button', { name: '移除跳板 nas-vpn' }));
    expect(props.onRemoveViaDevice).toHaveBeenCalledWith('dev-b');
  });

  test('toggles allow switch and reports aria-checked state', async () => {
    const user = userEvent.setup();
    const { props } = renderCard({ allowEnabled: false });
    const switchControl = screen.getByRole('switch', { name: '允许其他设备经本机中转' });
    expect(switchControl.getAttribute('aria-checked')).toBe('false');
    await user.click(switchControl);
    expect(props.onToggleAllow).toHaveBeenCalledWith(true);
  });

  test('shows save success as status and save failure as alert', () => {
    renderCard({ saveSuccess: '中转配置已保存' });
    expect(screen.getByRole('status').textContent).toContain('中转配置已保存');
    cleanup();
    renderCard({ saveError: '保存失败' });
    expect(screen.getByRole('alert').textContent).toContain('保存失败');
  });

  test('shows load failure with refresh retry', async () => {
    const user = userEvent.setup();
    const { props } = renderCard({ loadError: 'connection refused' });
    expect(screen.getByRole('alert').textContent).toContain('connection refused');
    await user.click(screen.getByRole('button', { name: '刷新' }));
    expect(props.onRefresh).toHaveBeenCalled();
  });
});
