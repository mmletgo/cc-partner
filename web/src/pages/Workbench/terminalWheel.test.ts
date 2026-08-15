import { describe, expect, test } from 'vitest';
import {
  consumeWorkbenchTerminalWheelLines,
  encodeTerminalSgrWheelReports,
  resolveWorkbenchTerminalWheelAction,
} from './terminalWheel';

describe('resolveWorkbenchTerminalWheelAction', () => {
  test('hydrates an ordinary tmux shell once before trusting local scrollback', () => {
    expect(
      resolveWorkbenchTerminalWheelAction({
        bufferType: 'normal',
        mouseTrackingMode: 'none',
        historyHydrated: false,
      }),
    ).toBe('hydrateScrollback');
    expect(
      resolveWorkbenchTerminalWheelAction({
        bufferType: 'normal',
        mouseTrackingMode: 'none',
        historyHydrated: true,
      }),
    ).toBe('scrollback');
  });

  test('does not trust polluted baseY before the active Agent history is hydrated', () => {
    expect(
      resolveWorkbenchTerminalWheelAction({
        bufferType: 'normal',
        mouseTrackingMode: 'none',
        historyHydrated: false,
      }),
    ).toBe('hydrateScrollback');
    expect(
      resolveWorkbenchTerminalWheelAction({
        bufferType: 'normal',
        mouseTrackingMode: 'none',
        historyHydrated: true,
      }),
    ).toBe('scrollback');
    expect(
      resolveWorkbenchTerminalWheelAction({
        bufferType: 'alternate',
        mouseTrackingMode: 'none',
        historyHydrated: false,
      }),
    ).toBe('hydrateScrollback');
  });

  test('routes a mouse-tracked normal buffer to Claude instead of replaying local redraw frames', () => {
    expect(
      resolveWorkbenchTerminalWheelAction({
        bufferType: 'normal',
        mouseTrackingMode: 'vt200',
        historyHydrated: false,
      }),
    ).toBe('sgrFallback');
  });

  test('non-agent alternate screen hydrates without mouse tracking and uses SGR once negotiated', () => {
    expect(
      resolveWorkbenchTerminalWheelAction({
        bufferType: 'alternate',
        mouseTrackingMode: 'none',
        historyHydrated: false,
      }),
    ).toBe('hydrateScrollback');
    expect(
      resolveWorkbenchTerminalWheelAction({
        bufferType: 'alternate',
        mouseTrackingMode: 'vt200',
        historyHydrated: false,
      }),
    ).toBe('sgrFallback');
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
