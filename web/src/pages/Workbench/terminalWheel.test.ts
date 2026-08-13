import { describe, expect, test } from 'vitest';
import {
  clampTranscriptWheelCell,
  consumeWorkbenchTerminalWheelLines,
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

  test('always injects transcript-targeted SGR on the alternate screen', () => {
    // Claude 输入框聚焦时 PageUp 不在 Chat 上下文；SGR 按落点命中。
    // 指针在底部输入区时必须自己发打在 transcript 的 SGR，不能交给 xterm 原坐标。
    expect(
      resolveWorkbenchTerminalWheelAction({
        bufferType: 'alternate',
        baseY: 0,
        mouseTrackingMode: 'none',
      }),
    ).toBe('sgrFallback');
    expect(
      resolveWorkbenchTerminalWheelAction({
        bufferType: 'alternate',
        baseY: 0,
        mouseTrackingMode: 'vt200',
      }),
    ).toBe('sgrFallback');
  });
});

describe('clampTranscriptWheelCell', () => {
  test('keeps a mid-screen hit and lifts a bottom-input hit into the transcript', () => {
    expect(clampTranscriptWheelCell(10, 8, 80, 32)).toEqual({ col: 10, row: 8 });
    expect(clampTranscriptWheelCell(40, 31, 80, 32)).toEqual({ col: 40, row: 24 });
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
  test('maps wheel-up to SGR 64 and wheel-down to SGR 65, never arrow keys or PageUp', () => {
    expect(encodeTerminalSgrWheelReports(-1, 10, 5)).toBe('\x1b[<64;10;5M');
    expect(encodeTerminalSgrWheelReports(2, 3, 7)).toBe('\x1b[<65;3;7M\x1b[<65;3;7M');
    const payload = encodeTerminalSgrWheelReports(-3, 1, 1);
    expect(payload.includes('\x1b[A') || payload.includes('\x1bOA')).toBe(false);
    expect(payload.includes('\x1b[5~')).toBe(false);
  });
});
