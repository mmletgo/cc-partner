import { describe, expect, test, vi } from 'vitest';
import type { Terminal } from '@xterm/xterm';
import { installWorkbenchTerminalSelectionOverrides } from './terminalSelectionOverrides';

describe('installWorkbenchTerminalSelectionOverrides', () => {
  test('no-ops safely when terminal has no internal selection service', () => {
    const terminal = {} as Terminal;
    const restore = installWorkbenchTerminalSelectionOverrides(terminal);
    expect(() => restore()).not.toThrow();
  });

  test('forces selection and ignores mouse-mode disable so copy selection survives', () => {
    // Business Logic: Claude/TUI mouse tracking 必须不能清掉/禁止 xterm 文本选区。
    const enable = vi.fn();
    const originalDisable = vi.fn();
    // Production signature is (event: MouseEvent) => boolean; restore must keep it.
    const originalShouldForce = vi.fn((event: MouseEvent) => {
      void event;
      return false;
    });
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
    expect(service.shouldForceSelection({ altKey: false } as MouseEvent)).toBe(true);

    service.disable();
    expect(originalDisable).not.toHaveBeenCalled();

    restore();
    // restore 后回到原始签名（需要 event）；再测禁用路径。
    expect(service.shouldForceSelection({ altKey: false } as MouseEvent)).toBe(false);
    service.disable();
    expect(originalDisable).toHaveBeenCalledTimes(1);
  });
});
