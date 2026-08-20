/**
 * 移动端钩子失败输出拼接测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   修复卡片必须稳定展示 stdout/stderr，不能把空端渲染成多余空行。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖双端、单端、全空三种输入。
 */

import { describe, expect, test } from 'vitest';
import { formatMobileHookRepairOutput } from './mobileHookRepair';

describe('formatMobileHookRepairOutput', () => {
  test('joins stdout and stderr with a newline', () => {
    expect(
      formatMobileHookRepairOutput({ stdout: 'lint failed\n', stderr: ' trailing space \n' }),
    ).toBe('lint failed\ntrailing space');
  });

  test('keeps the non-empty stream when the other is blank', () => {
    expect(formatMobileHookRepairOutput({ stdout: '', stderr: 'boom' })).toBe('boom');
    expect(formatMobileHookRepairOutput({ stdout: 'ok', stderr: '  ' })).toBe('ok');
  });

  test('returns empty string when both streams are blank', () => {
    expect(formatMobileHookRepairOutput({ stdout: '  ', stderr: '' })).toBe('');
  });
});
