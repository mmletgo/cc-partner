/**
 * config/permissions schema fixtures。
 */

import { describe, expect, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import { appConfigDecoder, permissionsStatusDecoder } from './config';

const validConfig = {
  deviceId: 'd1',
  deviceName: 'Mac',
  receiveDir: '/tmp/recv',
  screenshotHotkey: '<cmd>+s',
  promptOptimizerHotkey: '<ctrl>',
  promptOptimizerFillLanguage: 'zh',
  httpPort: 0,
};

describe('config schemas', () => {
  test('decodes app config', () => {
    expect(appConfigDecoder.decode(validConfig).deviceId).toBe('d1');
  });

  test('permissions default notification when missing', () => {
    expect(
      permissionsStatusDecoder.decode({
        screenCapture: { granted: true },
        inputMonitoring: { granted: false },
        accessibility: { granted: true },
      }),
    ).toEqual({
      screenCapture: { granted: true },
      inputMonitoring: { granted: false },
      accessibility: { granted: true },
      notification: { granted: false },
    });
  });

  test('malformed fill language fails', () => {
    expect(() =>
      appConfigDecoder.decode({ ...validConfig, promptOptimizerFillLanguage: 'jp' }),
    ).toThrow(ContractDecodeError);
  });
});
