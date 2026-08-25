/**
 * WorkbenchBatteryBadge 徽标合同测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   工作台标题旁的电池模式徽标是用户判断自我约束状态的第一眼信息：
 *   充电模式必须展示余额比例与剩余时长，低电量要转 warn/danger，
 *   无限模式只展示 ∞ 且不再有进度条，加载中不占位。
 *
 * Code Logic（这个测试做什么）:
 *   vi.hoisted 可变对象控制 useBattery 返回快照；
 *   I18nextProvider + zh 断言 data-mode/data-tone/aria-valuenow/文案；
 *   snapshot 为 null 时渲染为空。
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import { ExperimentalFeaturesProvider } from '@/hooks/useExperimentalFeatures';
import i18n from '@/i18n';
import type { BatterySnapshot } from '@/lib/types/battery';
import { DEFAULT_EXPERIMENTAL_FEATURES } from '@/lib/types/settings';

const batteryMock = vi.hoisted(() => ({
  snapshot: null as BatterySnapshot | null,
}));

vi.mock('@/hooks/useBattery', () => ({
  useBattery: () => batteryMock,
}));

import { WorkbenchBatteryBadge } from './WorkbenchBatteryBadge';

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   每个用例只关心差异字段，但快照合同要求全字段齐全，避免 mock 漂移。
 *
 * Code Logic（这个函数做什么）:
 *   以 charging 零余额为基线合并 overrides，返回完整 BatterySnapshot。
 */
function makeSnapshot(overrides: Partial<BatterySnapshot>): BatterySnapshot {
  return {
    mode: 'charging',
    remainingMs: 0,
    maxBalanceMs: 0,
    todayEarnedMs: 0,
    todaySpentMs: 0,
    consuming: false,
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   组件依赖 i18n 的 battery 命名空间文案。
 *
 * Code Logic（这个函数做什么）:
 *   用 I18nextProvider 挂载 WorkbenchBatteryBadge 并返回 render 结果。
 */
function renderBadge() {
  return render(
    <ExperimentalFeaturesProvider
      initialFeatures={{ ...DEFAULT_EXPERIMENTAL_FEATURES, battery: true }}
    >
      <I18nextProvider i18n={i18n}>
        <WorkbenchBatteryBadge />
      </I18nextProvider>
    </ExperimentalFeaturesProvider>,
  );
}

describe('WorkbenchBatteryBadge', () => {
  test('charging renders accent badge with progressbar percent and time label', () => {
    batteryMock.snapshot = makeSnapshot({
      remainingMs: 23 * 60_000,
      maxBalanceMs: 240 * 60_000,
    });
    renderBadge();

    const badge = screen.getByTestId('workbench-battery-badge');
    expect(badge.getAttribute('data-mode')).toBe('charging');
    expect(badge.getAttribute('data-tone')).toBe('accent');

    const bar = screen.getByRole('progressbar');
    expect(bar.getAttribute('aria-valuenow')).toBe('10');
    expect(bar.getAttribute('aria-valuemin')).toBe('0');
    expect(bar.getAttribute('aria-valuemax')).toBe('100');
    expect(bar.getAttribute('aria-label')).toBe('充电模式');
    expect(badge.textContent).toContain('23 分');
    expect(badge.getAttribute('title')).toBe('充电模式 · 剩余 23 分');
  });

  test('low remaining flips data-tone to warn then danger', () => {
    batteryMock.snapshot = makeSnapshot({
      remainingMs: 3 * 60_000,
      maxBalanceMs: 240 * 60_000,
    });
    renderBadge();
    expect(screen.getByTestId('workbench-battery-badge').getAttribute('data-tone')).toBe('warn');
    cleanup();

    batteryMock.snapshot = makeSnapshot({
      remainingMs: 0,
      maxBalanceMs: 240 * 60_000,
    });
    renderBadge();
    expect(screen.getByTestId('workbench-battery-badge').getAttribute('data-tone')).toBe('danger');
  });

  test('unlimited renders infinity img without progressbar', () => {
    batteryMock.snapshot = makeSnapshot({
      mode: 'unlimited',
      remainingMs: 240 * 60_000,
      maxBalanceMs: 240 * 60_000,
    });
    renderBadge();

    expect(screen.queryByRole('progressbar')).toBeNull();
    const badge = screen.getByTestId('workbench-battery-badge');
    expect(badge.getAttribute('data-mode')).toBe('unlimited');
    expect(screen.getByRole('img', { name: '无限模式' })).toBe(badge);
  });

  test('null snapshot renders nothing', () => {
    batteryMock.snapshot = null;
    const { container } = renderBadge();
    expect(container.firstChild).toBeNull();
  });
});
