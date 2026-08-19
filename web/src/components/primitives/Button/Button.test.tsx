// @vitest-environment jsdom
/**
 * Button 原语 loading 尺寸合同。
 *
 * Business Logic（为什么需要这些测试）:
 *   Dialog 确认按钮进入 applying 后不得因插入 spinner 改变宽高或 accessible name，
 *   否则「确认应用」与 loading 态会对不齐。
 *
 * Code Logic（这些测试做什么）:
 *   断言 loading 保留原文案、spinner 不进 accessible name、点击被短路。
 */

import { afterEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { Button } from './Button';

afterEach(() => {
  cleanup();
});

describe('Button loading', () => {
  test('keeps the original accessible name while overlaying a spinner', () => {
    render(
      <Button variant="primary" size="sm" loading>
        确认应用
      </Button>,
    );

    const button = screen.getByRole('button', { name: '确认应用' });
    expect(button.getAttribute('data-loading')).toBe('true');
    expect(button.textContent).toBe('确认应用');
    expect(button.querySelector('[aria-hidden="true"]')).toBeTruthy();
  });

  test('does not fire onClick while loading', () => {
    const onClick = vi.fn();
    render(
      <Button variant="primary" size="sm" loading onClick={onClick}>
        确认应用
      </Button>,
    );

    fireEvent.click(screen.getByRole('button', { name: '确认应用' }));
    expect(onClick).not.toHaveBeenCalled();
  });
});
