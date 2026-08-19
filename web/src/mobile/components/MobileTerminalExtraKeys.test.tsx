// @vitest-environment jsdom
/**
 * MobileTerminalExtraKeys 事件触发路径测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   输入法/软键盘激活时，移动浏览器首次 tap 的 click 常被吞（用于收键盘/取消 IME 组合）；
 *   按键主路径必须在 pointerDown 触发，否则用户在打字态下点快捷键会"无效"。
 *
 * Code Logic（这个测试做什么）:
 *   在 jsdom 下渲染 extra keys，分别模拟 pointerDown-only、pointerDown+click、
 *   仅 click（键盘兜底）与 disabled 四条路径，断言 onKeyPress 调用次数。
 */

import { afterEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, fireEvent, render } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import type { MobileTerminalExtraKeyDef } from '../mobileTerminalExtraKeys';
import { MOBILE_TERMINAL_EXTRA_KEY_LONG_PRESS_MS } from '../mobileTerminalExtraKeys';
import { MobileTerminalExtraKeys } from './MobileTerminalExtraKeys';

function renderExtraKeys(
  onKeyPress: (key: MobileTerminalExtraKeyDef) => void,
  disabled = false,
): { container: HTMLElement; esc: HTMLButtonElement; slash: HTMLButtonElement } {
  const { container } = render(
    <I18nextProvider i18n={i18n}>
      <MobileTerminalExtraKeys
        disabled={disabled}
        stickyModifier={null}
        onKeyPress={onKeyPress}
      />
    </I18nextProvider>,
  );
  const esc = container.querySelector('[data-key-id="esc"]') as HTMLButtonElement | null;
  const slash = container.querySelector('[data-key-id="slash"]') as HTMLButtonElement | null;
  if (!esc) throw new Error('esc button not rendered');
  if (!slash) throw new Error('slash button not rendered');
  return { container, esc, slash };
}

function mockRect(
  el: Element,
  box: { left: number; top: number; width: number; height: number },
): void {
  vi.spyOn(el, 'getBoundingClientRect').mockReturnValue({
    x: box.left,
    y: box.top,
    left: box.left,
    top: box.top,
    width: box.width,
    height: box.height,
    right: box.left + box.width,
    bottom: box.top + box.height,
    toJSON() {
      return this;
    },
  });
}

describe('MobileTerminalExtraKeys', () => {
  afterEach(() => {
    vi.useRealTimers();
    cleanup();
  });

  test('pointerDown 触发按键；click 被吞（IME 激活）时仍生效，连点不漏', () => {
    const onKeyPress = vi.fn();
    const { esc } = renderExtraKeys(onKeyPress);

    // 首次按下：模拟移动端在 IME/键盘激活时 click 不生成的场景。
    fireEvent.pointerDown(esc);
    expect(onKeyPress).toHaveBeenCalledTimes(1);

    // click 被吞（不派发），用户再次按下同一键仍应生效。
    fireEvent.pointerDown(esc);
    expect(onKeyPress).toHaveBeenCalledTimes(2);
  });

  test('pointerDown 后到达的 click 不重复触发按键', () => {
    const onKeyPress = vi.fn();
    const { esc } = renderExtraKeys(onKeyPress);

    fireEvent.pointerDown(esc);
    fireEvent.click(esc);
    expect(onKeyPress).toHaveBeenCalledTimes(1);
  });

  test('无 pointerDown 前导的 click（键盘 Enter/Space）兜底触发按键', () => {
    const onKeyPress = vi.fn();
    const { esc } = renderExtraKeys(onKeyPress);

    fireEvent.click(esc);
    expect(onKeyPress).toHaveBeenCalledTimes(1);
  });

  test('disabled 时 pointerDown 与 click 都不触发按键', () => {
    const onKeyPress = vi.fn();
    const { esc } = renderExtraKeys(onKeyPress, true);

    fireEvent.pointerDown(esc);
    fireEvent.click(esc);
    expect(onKeyPress).not.toHaveBeenCalled();
  });

  test('不再渲染翻页键（page kind 已从数据层移除）', () => {
    const onKeyPress = vi.fn();
    const { container } = renderExtraKeys(onKeyPress);
    // 历史 PAGE 1 / PAGE 2 已合并为单一横向滚动序列；用户通过滑动浏览全部键。
    // data-kind="page" 仍残留说明组件 props 或数据层未彻底收敛，需要排查。
    expect(container.querySelector('[data-kind="page"]')).toBeNull();
    expect(container.querySelector('[data-key-id="page-1"]')).toBeNull();
    expect(container.querySelector('[data-key-id="page-2"]')).toBeNull();
  });

  test('slash pointerDown 不立即发送；短按抬手才插入 /', () => {
    vi.useFakeTimers();
    const onKeyPress = vi.fn();
    const { slash } = renderExtraKeys(onKeyPress);

    fireEvent.pointerDown(slash, { pointerId: 1, clientX: 110, clientY: 420 });
    expect(onKeyPress).not.toHaveBeenCalled();
    expect(document.querySelector('[data-popup-item-id="slash-rewind"]')).toBeNull();

    fireEvent.pointerUp(slash, { pointerId: 1, clientX: 110, clientY: 420 });
    expect(onKeyPress).toHaveBeenCalledTimes(1);
    expect(onKeyPress.mock.calls[0]?.[0]?.id).toBe('slash');
    vi.useRealTimers();
  });

  test('slash 长按弹出三项；滑到 /rewind 松手插入该命令', () => {
    vi.useFakeTimers();
    const onKeyPress = vi.fn();
    const { slash } = renderExtraKeys(onKeyPress);
    mockRect(slash, { left: 100, top: 400, width: 48, height: 48 });

    fireEvent.pointerDown(slash, { pointerId: 1, clientX: 120, clientY: 420 });
    act(() => {
      vi.advanceTimersByTime(MOBILE_TERMINAL_EXTRA_KEY_LONG_PRESS_MS);
    });

    const rewind = document.querySelector('[data-popup-item-id="slash-rewind"]');
    const resume = document.querySelector('[data-popup-item-id="slash-resume"]');
    const compact = document.querySelector('[data-popup-item-id="slash-compact"]');
    expect(rewind).toBeTruthy();
    expect(resume).toBeTruthy();
    expect(compact).toBeTruthy();
    if (!rewind) throw new Error('rewind popup missing');
    mockRect(rewind, { left: 80, top: 340, width: 88, height: 48 });

    fireEvent.pointerMove(slash, { pointerId: 1, clientX: 120, clientY: 360 });
    fireEvent.pointerUp(slash, { pointerId: 1, clientX: 120, clientY: 360 });
    expect(onKeyPress).toHaveBeenCalledTimes(1);
    expect(onKeyPress.mock.calls[0]?.[0]?.id).toBe('slash-rewind');
    expect(onKeyPress.mock.calls[0]?.[0]?.payload).toBe('/rewind');
    expect(document.querySelector('[data-popup-item-id="slash-rewind"]')).toBeNull();
    vi.useRealTimers();
  });

  test('slash 无 pointer 前导的 click 仍立即插入 /', () => {
    const onKeyPress = vi.fn();
    const { slash } = renderExtraKeys(onKeyPress);
    fireEvent.click(slash);
    expect(onKeyPress).toHaveBeenCalledTimes(1);
    expect(onKeyPress.mock.calls[0]?.[0]?.id).toBe('slash');
  });

  test('slash pointerCancel 不发送任何键', () => {
    vi.useFakeTimers();
    const onKeyPress = vi.fn();
    const { slash } = renderExtraKeys(onKeyPress);
    fireEvent.pointerDown(slash, { pointerId: 1, clientX: 110, clientY: 420 });
    act(() => {
      vi.advanceTimersByTime(MOBILE_TERMINAL_EXTRA_KEY_LONG_PRESS_MS);
    });
    fireEvent.pointerCancel(slash, { pointerId: 1 });
    expect(onKeyPress).not.toHaveBeenCalled();
    expect(document.querySelector('[data-popup-item-id="slash-rewind"]')).toBeNull();
    vi.useRealTimers();
  });
});
