/**
 * ContextMeter 单元测试。
 *
 * Business Logic（为什么需要这个测试文件）:
 *   用量必须按 occupancy 用 k 展示，长度单独显示模型窗口；二者缺失路径不得假装精确。
 *
 * Code Logic（这个测试文件做什么）:
 *   - used=null → 用量「—」，无 ProgressBar，长度仍可显示；
 *   - window=null + used 有值 → 用量 k，长度 noWindowLabel，无 ProgressBar；
 *   - 两者都有 → ProgressBar + 百分比，窗口为整数 k。
 */
// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';

import { ContextMeter, type ContextMeterProps } from './ContextMeter';

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

function renderMeter(
  props: Pick<ContextMeterProps, 'contextUsed' | 'contextWindow' | 'tone'>,
) {
  return render(
    <I18nextProvider i18n={i18n}>
      <ContextMeter {...props} unavailableLabel="—" noWindowLabel="无窗口信息" />
    </I18nextProvider>,
  );
}

describe('ContextMeter', () => {
  it('contextUsed=null 时用量为 unavailableLabel，无 ProgressBar，长度仍显示', () => {
    const { container } = renderMeter({ contextUsed: null, contextWindow: 200_000, tone: 'accent' });
    const meter = screen.getByTestId('workbench-status-context-meter');
    expect(meter.textContent).toContain('—');
    expect(meter.textContent).toContain('200k');
    expect(container.querySelector('[role="progressbar"]')).toBeNull();
  });

  it('contextWindow=null + used 有值时显示 k 用量与 noWindowLabel，无 ProgressBar', () => {
    const { container } = renderMeter({ contextUsed: 50_000, contextWindow: null, tone: 'accent' });
    const meter = screen.getByTestId('workbench-status-context-meter');
    expect(meter.textContent).toContain('50.0k');
    expect(meter.textContent).toContain('无窗口信息');
    expect(meter.textContent).not.toMatch(/M/);
    expect(container.querySelector('[role="progressbar"]')).toBeNull();
  });

  it('两者都有时渲染 ProgressBar、百分比与整数窗口', () => {
    const { container } = renderMeter({
      contextUsed: 60_000,
      contextWindow: 200_000,
      tone: 'warn',
    });
    const bar = container.querySelector('[role="progressbar"]');
    expect(bar).not.toBeNull();
    expect(bar?.getAttribute('aria-valuenow')).toBe('30');
    expect(bar?.getAttribute('data-tone')).toBe('warn');
    const text = screen.getByTestId('workbench-status-context-meter').textContent ?? '';
    expect(text).toContain('30%');
    expect(text).toContain('60.0k');
    expect(text).toContain('200k');
    expect(text).not.toMatch(/M/);
  });

  it('百分比 >= 85 时传入 danger tone 透传', () => {
    const { container } = renderMeter({
      contextUsed: 180_000,
      contextWindow: 200_000,
      tone: 'danger',
    });
    expect(container.querySelector('[role="progressbar"]')?.getAttribute('data-tone')).toBe(
      'danger',
    );
  });
});
