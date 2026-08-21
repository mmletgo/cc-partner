/**
 * config/permissions schema fixtures。
 */

import { describe, expect, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import {
  appConfigDecoder,
  permissionActionResultDecoder,
  permissionsStatusDecoder,
} from './config';

const validConfig = {
  deviceId: 'd1',
  deviceName: 'Mac',
  receiveDir: '/tmp/recv',
  gamePluginDir: '/tmp/plugins',
  screenshotHotkey: '<cmd>+s',
  promptOptimizerHotkey: '<ctrl>',
  promptOptimizerFillLanguage: 'zh',
  promptOptimizerProvider: 'claude',
  promptQuickInputHotkey: '<ctrl>+/',
  httpPort: 0,
};

describe('config schemas', () => {
  test('defaults experimentalFeatures to all off when missing', () => {
    expect(appConfigDecoder.decode(validConfig).experimentalFeatures).toEqual({
      battery: false,
      game: false,
      browser: false,
      automation: false,
      cloudSync: false,
    });
  });


  test('rejects missing gamePluginDir', () => {
    const { gamePluginDir: _omitted, ...rest } = validConfig;
    expect(() => appConfigDecoder.decode(rest)).toThrow(ContractDecodeError);
    void _omitted;
  });

  test('permissions default notification when missing', () => {
    expect(
      permissionsStatusDecoder.decode({
        screenCapture: { granted: true },
        inputMonitoring: { granted: false, state: 'notDetermined' },
        accessibility: { granted: true },
      }),
    ).toEqual({
      screenCapture: { granted: true },
      inputMonitoring: { granted: false, state: 'notDetermined' },
      accessibility: { granted: true },
      notification: { granted: false },
    });
  });

  test('permissions preserve the input monitoring four-state contract', () => {
    const decoded = permissionsStatusDecoder.decode({
      screenCapture: { granted: true },
      inputMonitoring: { granted: false, state: 'unavailable' },
      accessibility: { granted: true },
      notification: { granted: true },
    });

    expect(decoded.inputMonitoring).toEqual({
      granted: false,
      state: 'unavailable',
    });
  });

  test('decodes explicit permission operations', () => {
    expect(
      permissionActionResultDecoder.decode({
        permission: 'inputMonitoring',
        operation: 'request',
        before: 'notDetermined',
        after: 'denied',
      }),
    ).toEqual({
      permission: 'inputMonitoring',
      operation: 'request',
      before: 'notDetermined',
      after: 'denied',
    });
  });

  test('malformed fill language fails', () => {
    expect(() =>
      appConfigDecoder.decode({ ...validConfig, promptOptimizerFillLanguage: 'jp' }),
    ).toThrow(ContractDecodeError);
  });

  test('defaults missing optimizer provider to claude and rejects unknown', () => {
    const { promptOptimizerProvider: _omitted, ...rest } = validConfig;
    expect(appConfigDecoder.decode(rest).promptOptimizerProvider).toBe('claude');
    void _omitted;
    expect(() =>
      appConfigDecoder.decode({ ...validConfig, promptOptimizerProvider: 'gemini' }),
    ).toThrow(ContractDecodeError);
    expect(() =>
      appConfigDecoder.decode({ ...validConfig, promptOptimizerProvider: 'unknown' }),
    ).toThrow(ContractDecodeError);
  });
});
