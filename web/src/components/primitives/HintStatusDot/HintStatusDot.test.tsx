// @vitest-environment jsdom
/**
 * HintStatusDot 数字标点测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   count=0 必须保持原点语义；count>0 才放大写数字，等待优先于完成。
 *
 * Code Logic（这个测试做什么）:
 *   断言数字、tone 属性和 99+ 截断。
 */

import { afterEach, describe, expect, test } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';

import { HintStatusDot } from './HintStatusDot';

describe('HintStatusDot', () => {
  afterEach(() => {
    cleanup();
  });

  test('count 为 0 时不写数字，也不带 hint tone', () => {
    const { container } = render(
      <HintStatusDot count={0} tone={null} className="origin" data-status="running" />,
    );
    const el = container.querySelector('.origin');
    expect(el).toBeTruthy();
    expect(el?.textContent).toBe('');
    expect(el?.getAttribute('data-hint-tone')).toBeNull();
    expect(el?.getAttribute('data-status')).toBe('running');
  });

  test('waiting 优先显示黄色数字', () => {
    render(
      <HintStatusDot
        count={3}
        tone="wait"
        aria-label="3 个等待输入"
        data-testid="hint-dot"
      />,
    );
    const el = screen.getByTestId('hint-dot');
    expect(el.textContent).toBe('3');
    expect(el.getAttribute('data-hint-tone')).toBe('wait');
    expect(el.getAttribute('aria-label')).toBe('3 个等待输入');
  });

  test('仅完成时显示绿色数字，超过 99 显示 99+', () => {
    render(<HintStatusDot count={120} tone="complete" data-testid="hint-dot" />);
    const el = screen.getByTestId('hint-dot');
    expect(el.textContent).toBe('99+');
    expect(el.getAttribute('data-hint-tone')).toBe('complete');
  });
});
