// @vitest-environment jsdom
/**
 * ActivityStats 页面加载与图表合同。
 *
 * Business Logic（为什么需要这个测试）:
 *   活动统计独立成页后，用户必须在 /activity 看到应用/窗口排行，而不是回到健康提醒页。
 *
 * Code Logic（这个测试做什么）:
 *   mock healthApi.getDetail，渲染 ActivityStats，断言图表标题与窗口排行项。
 */

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import { MemoryRouter } from 'react-router-dom';

import i18n from '@/i18n';

const getDetailMock = vi.fn();

vi.mock('@/api/health', () => ({
  healthApi: {
    getDetail: (...args: unknown[]) => getDetailMock(...args),
  },
}));

import { ActivityStats } from './ActivityStats';

/**
 * Business Logic（为什么需要这个函数）:
 *   异步加载完成后需冲刷 microtask，才能断言图表已挂载。
 *
 * Code Logic（这个函数做什么）:
 *   在 act 内多次 await Promise.resolve。
 */
async function flushMicrotasks(times = 20): Promise<void> {
  for (let i = 0; i < times; i += 1) {
    await act(async () => {
      await Promise.resolve();
    });
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   契约测试需统一 i18n + router 挂载。
 *
 * Code Logic（这个函数做什么）:
 *   用 MemoryRouter + I18nextProvider 渲染活动统计页。
 */
function renderActivityStats() {
  return render(
    <MemoryRouter>
      <I18nextProvider i18n={i18n}>
        <ActivityStats />
      </I18nextProvider>
    </MemoryRouter>,
  );
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

beforeEach(() => {
  getDetailMock.mockReset();
  getDetailMock.mockResolvedValue({
    appUsage: [{ name: 'Code', minutes: 12 }],
    windowUsage: [{ name: 'main.rs — cc-partner', minutes: 8 }],
    hourly: Array.from({ length: 24 }, () => 0),
  });
});

afterEach(() => {
  cleanup();
});

describe('ActivityStats page', () => {
  test('renders activity charts from detail', async () => {
    renderActivityStats();
    await flushMicrotasks();

    expect(screen.getByText('活动统计')).toBeTruthy();
    expect(screen.getByText('窗口使用时长(前 8)')).toBeTruthy();
    expect(document.body.textContent).toMatch(/main\.rs — cc-partner/);
    expect(getDetailMock).toHaveBeenCalled();
  });
});
