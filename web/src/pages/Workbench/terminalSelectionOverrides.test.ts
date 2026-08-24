import { describe, expect, test, vi } from 'vitest';
import type { Terminal } from '@xterm/xterm';
import {
  installWorkbenchTerminalSelectionOverrides,
  shouldForceWorkbenchTerminalSelection,
} from './terminalSelectionOverrides';

describe('shouldForceWorkbenchTerminalSelection', () => {
  test('lets ordinary clicks reach TUI mouse tracking', () => {
    const original = vi.fn((event: MouseEvent) => {
      void event;
      return false;
    });
    expect(
      shouldForceWorkbenchTerminalSelection({ shiftKey: false } as MouseEvent, original),
    ).toBe(false);
    expect(original).toHaveBeenCalledTimes(1);
  });

  test('forces selection for Shift or the original Option gesture', () => {
    const original = vi.fn((event: MouseEvent) => Boolean(event.altKey));
    expect(
      shouldForceWorkbenchTerminalSelection({ shiftKey: true, altKey: false } as MouseEvent, original),
    ).toBe(true);
    expect(
      shouldForceWorkbenchTerminalSelection({ shiftKey: false, altKey: true } as MouseEvent, original),
    ).toBe(true);
    expect(
      shouldForceWorkbenchTerminalSelection({ shiftKey: false, altKey: false } as MouseEvent, original),
    ).toBe(false);
  });
});

describe('installWorkbenchTerminalSelectionOverrides', () => {
  test('no-ops safely when terminal has no internal selection service', () => {
    const terminal = {} as Terminal;
    const restore = installWorkbenchTerminalSelectionOverrides(terminal);
    expect(() => restore()).not.toThrow();
  });

  test('keeps copy selection available without swallowing TUI clicks', () => {
    // Business Logic: mouse-mode CSI 不得清选区；普通点击必须仍能发 mouse report。
    const enable = vi.fn();
    const originalDisable = vi.fn();
    const originalShouldForce = vi.fn((event: MouseEvent) => Boolean(event.altKey));
    const service = {
      shouldForceSelection: originalShouldForce,
      disable: originalDisable,
      enable,
    };
    const terminal = {
      _core: { _selectionService: service },
    } as unknown as Terminal;

    const restore = installWorkbenchTerminalSelectionOverrides(terminal);

    expect(enable).toHaveBeenCalledTimes(1);
    expect(service.shouldForceSelection({ altKey: false, shiftKey: false } as MouseEvent)).toBe(
      false,
    );
    expect(service.shouldForceSelection({ altKey: true, shiftKey: false } as MouseEvent)).toBe(true);
    expect(service.shouldForceSelection({ altKey: false, shiftKey: true } as MouseEvent)).toBe(true);

    service.disable();
    expect(originalDisable).not.toHaveBeenCalled();

    restore();
    expect(service.shouldForceSelection({ altKey: false, shiftKey: false } as MouseEvent)).toBe(
      false,
    );
    expect(service.shouldForceSelection({ altKey: true, shiftKey: false } as MouseEvent)).toBe(true);
    service.disable();
    expect(originalDisable).toHaveBeenCalledTimes(1);
  });
});
