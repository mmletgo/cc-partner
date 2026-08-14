// @vitest-environment jsdom
/**
 * HintStatusDot 数字标点测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   等待/已停止必须始终成对可见，0 也要写出来；等待优先着色。
 *
 * Code Logic（这个测试做什么）:
 *   断言 0/0 文本、tone 属性和 99+ 截断。
 */

import { afterEach, describe, expect, test } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';

import { HintStatusDot } from './HintStatusDot';

describe('HintStatusDot', () => {
  afterEach(() => {
    cleanup();
  });

  test('0/0 仍写出数字并带 zero tone', () => {
    render(
      <HintStatusDot
        waitingCount={0}
        stoppedCount={0}
        className="origin"
        data-status="running"
        data-testid="hint-dot"
      />,
    );
    const el = screen.getByTestId('hint-dot');
    expect(el.textContent).toBe('0/0');
    expect(el.getAttribute('data-hint-tone')).toBe('zero');
    expect(el.getAttribute('data-status')).toBe('running');
  });

  test('waiting 优先显示黄色，并保留已停止数字', () => {
    render(
      <HintStatusDot
        waitingCount={3}
        stoppedCount={1}
        aria-label="3 个等待输入，1 个已停止"
        data-testid="hint-dot"
      />,
    );
    const el = screen.getByTestId('hint-dot');
    expect(el.textContent).toBe('3/1');
    expect(el.getAttribute('data-hint-tone')).toBe('wait');
    expect(el.getAttribute('aria-label')).toBe('3 个等待输入，1 个已停止');
  });

  test('仅已停止时显示绿色数字，超过 99 显示 99+', () => {
    render(
      <HintStatusDot waitingCount={0} stoppedCount={120} data-testid="hint-dot" />,
    );
    const el = screen.getByTestId('hint-dot');
    expect(el.textContent).toBe('0/99+');
    expect(el.getAttribute('data-hint-tone')).toBe('complete');
  });
});
