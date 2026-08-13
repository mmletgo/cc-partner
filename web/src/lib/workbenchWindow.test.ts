/**
 * 多窗 Workbench 窗口身份与 auto slot 合同。
 *
 * Business Logic（为什么需要这个测试）:
 *   主窗 / 卫星窗 / overlay 的 label 与 layout slot 必须前后端一致，
 *   否则后续多窗 restore 会互相覆盖 desktop:auto。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 main、workbench-1..4、overlay 前缀与非法 label 的 role/slot 解析。
 */

import { describe, expect, test } from 'vitest';

import {
  MAIN_WINDOW_LABEL,
  MAX_WORKBENCH_SATELLITE_WINDOWS,
  WORKBENCH_WINDOW_LABEL_PREFIX,
  isWindowAutoSlotKey,
  layoutSlotKeyForWindowLabel,
  parseSatelliteSlot,
  parseWorkbenchWindowRole,
  satelliteWindowLabel,
} from './workbenchWindow';

describe('workbenchWindow identity contract', () => {
  test('exports stable main label, prefix and satellite cap', () => {
    expect(MAIN_WINDOW_LABEL).toBe('main');
    expect(WORKBENCH_WINDOW_LABEL_PREFIX).toBe('workbench-');
    expect(MAX_WORKBENCH_SATELLITE_WINDOWS).toBe(4);
  });

  test('parseWorkbenchWindowRole classifies main / satellite / overlay', () => {
    expect(parseWorkbenchWindowRole('main')).toBe('main');
    expect(parseWorkbenchWindowRole('workbench-1')).toBe('satellite');
    expect(parseWorkbenchWindowRole('workbench-4')).toBe('satellite');
    expect(parseWorkbenchWindowRole('screenshot-overlay-0')).toBe('overlay');
    expect(parseWorkbenchWindowRole('health-overlay-1')).toBe('overlay');
    expect(parseWorkbenchWindowRole('')).toBe('overlay');
    expect(parseWorkbenchWindowRole('unknown')).toBe('overlay');
    expect(parseWorkbenchWindowRole('workbench-5')).toBe('overlay');
    expect(parseWorkbenchWindowRole('workbench-0')).toBe('overlay');
    expect(parseWorkbenchWindowRole('workbench-01')).toBe('overlay');
  });

  test('satelliteWindowLabel and parseSatelliteSlot are inverses for 1..4', () => {
    const slots = [1, 2, 3, 4] as const;
    for (const slot of slots) {
      const label = satelliteWindowLabel(slot);
      expect(label).toBe(`workbench-${slot}`);
      expect(parseSatelliteSlot(label)).toBe(slot);
    }
    expect(parseSatelliteSlot('main')).toBeNull();
    expect(parseSatelliteSlot('screenshot-overlay-0')).toBeNull();
    expect(parseSatelliteSlot('workbench-5')).toBeNull();
    expect(parseSatelliteSlot('workbench-0')).toBeNull();
    expect(parseSatelliteSlot('workbench-01')).toBeNull();
    expect(parseSatelliteSlot('workbench-')).toBeNull();
  });

  test('layoutSlotKeyForWindowLabel maps main and satellites only', () => {
    expect(layoutSlotKeyForWindowLabel('main')).toBe('desktop:auto');
    expect(layoutSlotKeyForWindowLabel('workbench-2')).toBe(
      'desktop:auto:window:workbench-2',
    );
    expect(layoutSlotKeyForWindowLabel('workbench-1')).toBe(
      'desktop:auto:window:workbench-1',
    );
    expect(layoutSlotKeyForWindowLabel('workbench-4')).toBe(
      'desktop:auto:window:workbench-4',
    );
    expect(() => layoutSlotKeyForWindowLabel('screenshot-overlay-0')).toThrow();
    expect(() => layoutSlotKeyForWindowLabel('health-overlay-1')).toThrow();
    expect(() => layoutSlotKeyForWindowLabel('workbench-5')).toThrow();
    expect(() => layoutSlotKeyForWindowLabel('')).toThrow();
    expect(() => layoutSlotKeyForWindowLabel('unknown')).toThrow();
  });

  test('isWindowAutoSlotKey accepts main and satellite auto slots only', () => {
    expect(isWindowAutoSlotKey('desktop:auto')).toBe(true);
    expect(isWindowAutoSlotKey('desktop:auto:window:workbench-1')).toBe(true);
    expect(isWindowAutoSlotKey('desktop:auto:window:workbench-4')).toBe(true);
    expect(isWindowAutoSlotKey('desktop:other')).toBe(false);
    expect(isWindowAutoSlotKey('desktop:auto:window:workbench-5')).toBe(false);
    expect(isWindowAutoSlotKey('desktop:auto:window:workbench-01')).toBe(false);
    expect(isWindowAutoSlotKey('desktop:auto:window:main')).toBe(false);
    expect(isWindowAutoSlotKey('named:11111111-1111-4111-8111-111111111111')).toBe(
      false,
    );
  });
});
