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
import { cleanup, fireEvent, render } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import type { MobileTerminalExtraKeyDef } from '../mobileTerminalExtraKeys';
import { MobileTerminalExtraKeys } from './MobileTerminalExtraKeys';

function renderExtraKeys(
  onKeyPress: (key: MobileTerminalExtraKeyDef) => void,
  disabled = false,
): { esc: HTMLButtonElement } {
  const { container } = render(
    <I18nextProvider i18n={i18n}>
      <MobileTerminalExtraKeys
        disabled={disabled}
        page={1}
        stickyModifier={null}
        onKeyPress={onKeyPress}
      />
    </I18nextProvider>,
  );
  const esc = container.querySelector('[data-key-id="esc"]') as HTMLButtonElement | null;
  if (!esc) throw new Error('esc button not rendered');
  return { esc };
}

describe('MobileTerminalExtraKeys', () => {
  afterEach(() => {
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
});
