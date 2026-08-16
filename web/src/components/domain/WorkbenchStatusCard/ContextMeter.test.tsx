/**
 * ContextMeter 单元测试。
 *
 * Business Logic（为什么需要这个测试文件）:
 *   ContextMeter 是状态卡 4 个 agent 指标中最重要的百分比视图；
 *   contextWindow 缺失 / 已知 / 比例阈值是用户可见行为的根。
 *
 * Code Logic（这个测试文件做什么）:
 *   - cumulativeIn=null → 整行 unavailableLabel，无 ProgressBar；
 *   - contextWindow=null + cumulativeIn 有值 → 仅显示 cumulative + noWindowLabel，无 ProgressBar；
 *   - 两者都有 → ProgressBar 显示百分比，aria-label 含 pct。
 */
// @vitest-environment jsdom
import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { I18nextProvider } from 'react-i18next';
import i18next from 'i18next';

import { ContextMeter, type ContextMeterProps } from './ContextMeter';

async function setupI18n(): Promise<void> {
  await i18next.changeLanguage('zh-CN');
  if (!i18next.isInitialized) {
    await new Promise<void>((resolve) => {
      i18next.init({ lng: 'zh-CN', resources: {} }, () => resolve());
    });
  }
}

function renderMeter(
  props: Pick<ContextMeterProps, 'cumulativeIn' | 'contextWindow' | 'tone'>,
) {
  return render(
    <I18nextProvider i18n={i18next}>
      <ContextMeter {...props} unavailableLabel="—" noWindowLabel="无窗口信息" />
    </I18nextProvider>,
  );
}

describe('ContextMeter', () => {
  it('cumulativeIn=null 时整行 unavailableLabel，无 ProgressBar', async () => {
    await setupI18n();
    const { container } = renderMeter({ cumulativeIn: null, contextWindow: 200_000, tone: 'accent' });
    const meter = screen.getByTestId('workbench-status-context-meter');
    expect(meter.textContent).toContain('—');
    expect(container.querySelector('[role="progressbar"]')).toBeNull();
  });

  it('contextWindow=null + cumulativeIn 有值时显示 noWindowLabel，无 ProgressBar', async () => {
    await setupI18n();
    const { container } = renderMeter({ cumulativeIn: 50_000, contextWindow: null, tone: 'accent' });
    const meter = screen.getByTestId('workbench-status-context-meter');
    expect(meter.textContent).toContain('50.000k');
    expect(meter.textContent).toContain('无窗口信息');
    expect(container.querySelector('[role="progressbar"]')).toBeNull();
  });

  it('两者都有时渲染 ProgressBar 与百分比', async () => {
    await setupI18n();
    const { container } = renderMeter({
      cumulativeIn: 60_000,
      contextWindow: 200_000,
      tone: 'warn',
    });
    const bar = container.querySelector('[role="progressbar"]');
    expect(bar).not.toBeNull();
    expect(bar?.getAttribute('aria-valuenow')).toBe('30');
    expect(bar?.getAttribute('data-tone')).toBe('warn');
    expect(screen.getByTestId('workbench-status-context-meter').textContent).toContain('30%');
  });

  it('百分比 >= 85 时传入 danger tone 透传', async () => {
    await setupI18n();
    const { container } = renderMeter({
      cumulativeIn: 180_000,
      contextWindow: 200_000,
      tone: 'danger',
    });
    expect(container.querySelector('[role="progressbar"]')?.getAttribute('data-tone')).toBe('danger');
  });
});