// @vitest-environment jsdom
/**
 * PointerPrimaryButton 触发路径测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   输入法/软键盘激活时移动浏览器首次 tap 的 click 常被吞，按钮主路径必须在 pointerDown 触发，
 *   同时保留键盘 click 兜底并防止 pointerDown+click 双触发。
 *
 * Code Logic（这个测试做什么）:
 *   渲染 PointerPrimaryButton，分别模拟 pointerDown-only、pointerDown+click、仅 click、disabled、
 *   beforePointerDown 五条路径，断言 onPrimary / beforePointerDown 调用次数。
 */

import { afterEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/react';

import { PointerPrimaryButton } from './PointerPrimaryButton';

function renderButton(
  onPrimary: () => void,
  props: { disabled?: boolean; beforePointerDown?: () => void } = {},
): HTMLButtonElement {
  const { container } = render(
    <PointerPrimaryButton onPrimary={onPrimary} {...props}>
      x
    </PointerPrimaryButton>,
  );
  const button = container.querySelector('button');
  if (!button) throw new Error('button not rendered');
  return button;
}

describe('PointerPrimaryButton', () => {
  afterEach(() => {
    cleanup();
  });

  test('pointerDown 触发 onPrimary；无 click 时仍生效（IME 吞 click），连点不漏', () => {
    const onPrimary = vi.fn();
    const button = renderButton(onPrimary);

    fireEvent.pointerDown(button);
    expect(onPrimary).toHaveBeenCalledTimes(1);

    fireEvent.pointerDown(button);
    expect(onPrimary).toHaveBeenCalledTimes(2);
  });

  test('pointerDown 后到达的 click 不重复触发', () => {
    const onPrimary = vi.fn();
    const button = renderButton(onPrimary);

    fireEvent.pointerDown(button);
    fireEvent.click(button);
    expect(onPrimary).toHaveBeenCalledTimes(1);
  });

  test('无 pointerDown 前导的 click 兜底触发（键盘 Enter/Space）', () => {
    const onPrimary = vi.fn();
    const button = renderButton(onPrimary);

    fireEvent.click(button);
    expect(onPrimary).toHaveBeenCalledTimes(1);
  });

  test('beforePointerDown 在 pointerDown 阶段执行', () => {
    const onPrimary = vi.fn();
    const before = vi.fn();
    const button = renderButton(onPrimary, { beforePointerDown: before });

    fireEvent.pointerDown(button);
    expect(before).toHaveBeenCalledTimes(1);
    expect(onPrimary).toHaveBeenCalledTimes(1);
  });

  test('disabled 时不触发 onPrimary', () => {
    const onPrimary = vi.fn();
    const button = renderButton(onPrimary, { disabled: true });

    fireEvent.pointerDown(button);
    fireEvent.click(button);
    expect(onPrimary).not.toHaveBeenCalled();
  });
});
