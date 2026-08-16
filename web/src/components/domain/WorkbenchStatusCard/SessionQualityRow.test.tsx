/**
 * SessionQualityRow 单元测试。
 *
 * Business Logic（为什么需要这个测试文件）:
 *   首 token 平均与缓存命中率是状态卡新增质量指标，null 必须显示「未提供」。
 *
 * Code Logic（这个测试文件做什么）:
 *   断言时长/百分比格式与 data-state。
 */
// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';

import { SessionQualityRow } from './SessionQualityRow';

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

function renderRow(props: { firstTokenAvgMs: number | null; cacheHitRate: number | null }) {
  return render(
    <I18nextProvider i18n={i18n}>
      <SessionQualityRow {...props} unavailableLabel="—" />
    </I18nextProvider>,
  );
}

describe('SessionQualityRow', () => {
  it('双 null 显示 unavailable', () => {
    renderRow({ firstTokenAvgMs: null, cacheHitRate: null });
    const row = screen.getByTestId('workbench-status-session-quality-row');
    expect(row.textContent).toContain('首 token 平均');
    expect(row.textContent).toContain('缓存命中率');
    const dds = row.querySelectorAll('dd');
    expect(dds[0]?.textContent).toBe('—');
    expect(dds[1]?.textContent).toBe('—');
  });

  it('有值时显示时长与百分比', () => {
    renderRow({ firstTokenAvgMs: 1800, cacheHitRate: 0.87 });
    const row = screen.getByTestId('workbench-status-session-quality-row');
    expect(row.textContent).toContain('1.80 s');
    expect(row.textContent).toContain('87.0%');
  });
});
