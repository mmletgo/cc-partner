/**
 * TokenRateRow 单元测试。
 *
 * Business Logic（为什么需要这个测试文件）:
 *   速率行是状态卡 4 个 agent 指标之一；null → 「未提供」降级是用户可见行为根。
 *
 * Code Logic（这个测试文件做什么）:
 *   - null speed → 显示 unavailableLabel（不是「0 tok/s」）；
 *   - 数值渲染走 formatTokenRate 约定（k/M 单位）；
 *   - data-state 属性区分 available/unavailable 供 e2e 抓取。
 */
// @vitest-environment jsdom
import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { I18nextProvider } from 'react-i18next';
import i18next from 'i18next';

import { TokenRateRow } from './TokenRateRow';

async function setupI18n(): Promise<void> {
  await i18next.changeLanguage('zh-CN');
  if (!i18next.isInitialized) {
    await new Promise<void>((resolve) => {
      i18next.init({ lng: 'zh-CN', resources: {} }, () => resolve());
    });
  }
}

function renderRow(props: { speedInTps: number | null; speedOutTps: number | null }) {
  return render(
    <I18nextProvider i18n={i18next}>
      <TokenRateRow {...props} unavailableLabel="—" />
    </I18nextProvider>,
  );
}

describe('TokenRateRow', () => {
  it('null 双值时显示 unavailableLabel', async () => {
    await setupI18n();
    renderRow({ speedInTps: null, speedOutTps: null });
    const dds = screen.getAllByTestId('workbench-status-token-rate-row')[0].querySelectorAll('dd');
    expect(dds).toHaveLength(2);
    expect(dds[0]?.textContent).toBe('—');
    expect(dds[1]?.textContent).toBe('—');
    expect(dds[0]?.getAttribute('data-state')).toBe('unavailable');
    expect(dds[1]?.getAttribute('data-state')).toBe('unavailable');
  });

  it('数值时走 formatTokenRate 约定（>5k → k 单位）', async () => {
    await setupI18n();
    renderRow({ speedInTps: 12345.6, speedOutTps: 6.5 });
    const container = screen.getByTestId('workbench-status-token-rate-row');
    expect(container.textContent).toContain('12.35k tok/s');
    expect(container.textContent).toContain('6.50 tok/s');
    const dds = container.querySelectorAll('dd');
    expect(dds[0]?.getAttribute('data-state')).toBe('available');
    expect(dds[1]?.getAttribute('data-state')).toBe('available');
  });

  it('仅单边 null 时另一边仍显示数值', async () => {
    await setupI18n();
    renderRow({ speedInTps: 100, speedOutTps: null });
    const dds = screen.getByTestId('workbench-status-token-rate-row').querySelectorAll('dd');
    expect(dds[0]?.textContent).toBe('100.0 tok/s');
    expect(dds[1]?.textContent).toBe('—');
    expect(dds[0]?.getAttribute('data-state')).toBe('available');
    expect(dds[1]?.getAttribute('data-state')).toBe('unavailable');
  });
});