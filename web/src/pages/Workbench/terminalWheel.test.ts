import { describe, expect, test } from 'vitest';
import {
  consumeWorkbenchTerminalWheelLines,
  encodeTerminalPageScrollKeys,
  encodeTerminalSgrWheelReports,
  resolveWorkbenchTerminalWheelAction,
} from './terminalWheel';

describe('resolveWorkbenchTerminalWheelAction', () => {
  test('uses local scrollback on the normal buffer even without history', () => {
    expect(
      resolveWorkbenchTerminalWheelAction({
        bufferType: 'normal',
        baseY: 0,
        mouseTrackingMode: 'none',
      }),
    ).toBe('scrollback');
    expect(
      resolveWorkbenchTerminalWheelAction({
        bufferType: 'normal',
        baseY: 12,
        mouseTrackingMode: 'none',
      }),
    ).toBe('scrollback');
  });

  test('lets xterm emit SGR when the TUI already enabled mouse tracking', () => {
    expect(
      resolveWorkbenchTerminalWheelAction({
        bufferType: 'alternate',
        baseY: 0,
        mouseTrackingMode: 'vt200',
      }),
    ).toBe('protocol');
  });

  test('falls back to PageUp/PageDown when Claude is on the alt screen without mouse tracking', () => {
    // Claude 在 tmux mouse off 时官方提示用 PgUp/PgDn 滚 transcript。
    // SGR 64/65 会走 mouse dispatch 打到输入框，看起来像“滚轮没反应”。
    expect(
      resolveWorkbenchTerminalWheelAction({
        bufferType: 'alternate',
        baseY: 0,
        mouseTrackingMode: 'none',
      }),
    ).toBe('pageFallback');
  });
});

describe('consumeWorkbenchTerminalWheelLines', () => {
  test('accumulates sub-cell trackpad deltas into whole lines', () => {
    const first = consumeWorkbenchTerminalWheelLines(-10, 0, 16);
    expect(first.lines).toBe(0);
    const second = consumeWorkbenchTerminalWheelLines(-10, first.remainder, 16);
    expect(second.lines).toBe(-1);
  });
});

describe('encodeTerminalSgrWheelReports', () => {
  test('maps wheel-up to SGR 64 and wheel-down to SGR 65, never arrow keys', () => {
    expect(encodeTerminalSgrWheelReports(-1, 10, 5)).toBe('\x1b[<64;10;5M');
    expect(encodeTerminalSgrWheelReports(2, 3, 7)).toBe('\x1b[<65;3;7M\x1b[<65;3;7M');
    const payload = encodeTerminalSgrWheelReports(-3, 1, 1);
    expect(payload.includes('\x1b[A') || payload.includes('\x1bOA')).toBe(false);
  });
});

describe('encodeTerminalPageScrollKeys', () => {
  test('maps wheel-up to PageUp and wheel-down to PageDown, never arrows or SGR', () => {
    expect(encodeTerminalPageScrollKeys(-1)).toBe('\x1b[5~');
    expect(encodeTerminalPageScrollKeys(2)).toBe('\x1b[6~');
    const payload = encodeTerminalPageScrollKeys(-3);
    expect(payload.includes('\x1b[A') || payload.includes('\x1bOA')).toBe(false);
    expect(payload.includes('\x1b[<64')).toBe(false);
  });
});
