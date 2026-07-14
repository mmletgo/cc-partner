/**
 * faultRecovery 故障分类 / 恢复策略 / 终端桥单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   L0 合同要求每个 FaultProfile 有 typed 分类、cache/stale/retry 与无乐观发散；
 *   终端 disconnect/reconnect 必须精确 listener 与 input counts。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 classifyTransportFault / planFaultRecovery / createTerminalFaultBridge；
 *   并用 BackendHarnessCore fault 行为产生真实 error 再分类以增强可信度。
 */

import { describe, expect, test } from 'vitest';

import {
  BackendHarnessCore,
  FAULT_PROFILE_CODES,
  createFaultError,
  faultProfileCode,
  type FaultProfile as HarnessFaultProfile,
} from '../../tests/support/backendHarness';
import {
  classifyTransportFault,
  createTerminalFaultBridge,
  faultProfileCodes,
  planFaultRecovery,
  type FaultProfile,
} from './faultRecovery';

const PROFILES: readonly FaultProfile[] = [
  'networkOffline',
  'timeout',
  'malformedJson',
  'permissionDenied',
  'conflict',
  'dbBusy',
  'lanBoundaryRejected',
  'crossSiteRejected',
];

/**
 * Business Logic（为什么需要这个函数）:
 *   各 profile 的 cache/retry 期望集中定义，避免散落 magic 断言。
 *
 * Code Logic（这个函数做什么）:
 *   返回 classification 关键字段的期望对象。
 */
function expectedPolicy(profile: FaultProfile): {
  code: string;
  retryable: boolean;
  showRetry: boolean;
  cachePolicy: 'keepStale' | 'clear' | 'none';
} {
  switch (profile) {
    case 'networkOffline':
    case 'timeout':
    case 'dbBusy':
      return {
        code: FAULT_PROFILE_CODES[profile],
        retryable: true,
        showRetry: true,
        cachePolicy: 'keepStale',
      };
    case 'malformedJson':
      return {
        code: FAULT_PROFILE_CODES.malformedJson,
        retryable: true,
        showRetry: true,
        cachePolicy: 'clear',
      };
    case 'permissionDenied':
    case 'conflict':
      return {
        code: FAULT_PROFILE_CODES[profile],
        retryable: true,
        showRetry: true,
        cachePolicy: 'none',
      };
    case 'lanBoundaryRejected':
    case 'crossSiteRejected':
      return {
        code: FAULT_PROFILE_CODES[profile],
        retryable: false,
        showRetry: false,
        cachePolicy: 'none',
      };
    default: {
      const _exhaustive: never = profile;
      throw new Error(String(_exhaustive));
    }
  }
}

describe('faultRecovery classifyTransportFault', () => {
  test.each(PROFILES)('classifies profile string %s with stable code and policies', (profile) => {
    const classification = classifyTransportFault(profile);
    const expected = expectedPolicy(profile);

    expect(classification.kind).toBe(profile);
    expect(classification.code).toBe(expected.code);
    expect(classification.retryable).toBe(expected.retryable);
    expect(classification.showRetry).toBe(expected.showRetry);
    expect(classification.cachePolicy).toBe(expected.cachePolicy);
    expect(classification.allowOptimisticCommit).toBe(false);
  });

  test.each(PROFILES)('classifies Error with harness code for %s', (profile) => {
    const code = faultProfileCode(profile as HarnessFaultProfile);
    const error = createFaultError('Error', `fault ${profile}`, code);
    const classification = classifyTransportFault(error);

    expect(classification.kind).toBe(profile);
    expect(classification.code).toBe(code);
    expect(classification.allowOptimisticCommit).toBe(false);
  });

  test('classifies AbortError as timeout', () => {
    const error = new Error('Aborted');
    error.name = 'AbortError';
    const classification = classifyTransportFault(error);
    expect(classification.kind).toBe('timeout');
    expect(classification.cachePolicy).toBe('keepStale');
    expect(classification.showRetry).toBe(true);
  });

  test('classifies Failed to fetch as networkOffline', () => {
    const error = new Error('Failed to fetch');
    error.name = 'TypeError';
    const classification = classifyTransportFault(error);
    expect(classification.kind).toBe('networkOffline');
    expect(classification.cachePolicy).toBe('keepStale');
  });

  test('classifies SyntaxError as malformedJson fail-closed', () => {
    const error = new SyntaxError('malformed JSON payload');
    const classification = classifyTransportFault(error);
    expect(classification.kind).toBe('malformedJson');
    expect(classification.cachePolicy).toBe('clear');
    expect(classification.showRetry).toBe(true);
  });

  test('aligns codes with harness FAULT_PROFILE_CODES', () => {
    expect(faultProfileCodes()).toEqual({ ...FAULT_PROFILE_CODES });
  });
});

describe('faultRecovery planFaultRecovery', () => {
  test.each(PROFILES)('no optimistic divergence for %s when optimistic applied', (profile) => {
    const classification = classifyTransportFault(profile);
    const plan = planFaultRecovery({
      classification,
      hasCache: true,
      optimisticApplied: true,
    });

    expect(plan.rollbackOptimistic).toBe(true);
    expect(plan.allowOptimisticCommit).toBe(false);
    expect(plan.noOptimisticDivergence).toBe(true);
    expect(plan.showRetry).toBe(classification.showRetry);
  });

  test('offline/timeout keep stale cache when present', () => {
    for (const profile of ['networkOffline', 'timeout'] as const) {
      const plan = planFaultRecovery({
        classification: classifyTransportFault(profile),
        hasCache: true,
        optimisticApplied: false,
      });
      expect(plan.keepCache).toBe(true);
      expect(plan.markStale).toBe(true);
      expect(plan.clearCache).toBe(false);
      expect(plan.rollbackOptimistic).toBe(false);
      expect(plan.showRetry).toBe(true);
    }
  });

  test('offline without cache does not invent cache', () => {
    const plan = planFaultRecovery({
      classification: classifyTransportFault('networkOffline'),
      hasCache: false,
      optimisticApplied: false,
    });
    expect(plan.keepCache).toBe(false);
    expect(plan.markStale).toBe(false);
  });

  test('malformedJson fail-closed clears cache', () => {
    const plan = planFaultRecovery({
      classification: classifyTransportFault('malformedJson'),
      hasCache: true,
      optimisticApplied: true,
    });
    expect(plan.clearCache).toBe(true);
    expect(plan.keepCache).toBe(false);
    expect(plan.markStale).toBe(false);
    expect(plan.rollbackOptimistic).toBe(true);
    expect(plan.allowOptimisticCommit).toBe(false);
  });

  test('permissionDenied keeps existing cache without stale and shows retry', () => {
    const plan = planFaultRecovery({
      classification: classifyTransportFault('permissionDenied'),
      hasCache: true,
      optimisticApplied: false,
    });
    expect(plan.keepCache).toBe(true);
    expect(plan.markStale).toBe(false);
    expect(plan.clearCache).toBe(false);
    expect(plan.showRetry).toBe(true);
  });

  test('lan/crossSite rejected do not mark stale or force clear, no retry', () => {
    for (const profile of ['lanBoundaryRejected', 'crossSiteRejected'] as const) {
      const plan = planFaultRecovery({
        classification: classifyTransportFault(profile),
        hasCache: true,
        optimisticApplied: true,
      });
      expect(plan.markStale).toBe(false);
      expect(plan.clearCache).toBe(false);
      expect(plan.showRetry).toBe(false);
      expect(plan.rollbackOptimistic).toBe(true);
      expect(plan.allowOptimisticCommit).toBe(false);
    }
  });
});

describe('faultRecovery harness-backed errors', () => {
  test('BackendHarnessCore fault profiles produce classifiable errors', async () => {
    const harness = new BackendHarnessCore();
    const invokeProfiles: FaultProfile[] = [
      'networkOffline',
      'malformedJson',
      'permissionDenied',
      'conflict',
      'dbBusy',
      'lanBoundaryRejected',
      'crossSiteRejected',
    ];

    for (const profile of invokeProfiles) {
      harness.command(`cmd_${profile}`, { kind: 'fault', profile });
      let caught: unknown;
      try {
        await harness.handleInvoke(`cmd_${profile}`);
      } catch (error) {
        caught = error;
      }
      expect(caught).toBeDefined();
      const classification = classifyTransportFault(caught);
      expect(classification.kind).toBe(profile);
      expect(classification.code).toBe(FAULT_PROFILE_CODES[profile]);
      expect(classification.allowOptimisticCommit).toBe(false);

      const plan = planFaultRecovery({
        classification,
        hasCache: true,
        optimisticApplied: true,
      });
      expect(plan.rollbackOptimistic).toBe(true);
      expect(plan.allowOptimisticCommit).toBe(false);
      expect(plan.noOptimisticDivergence).toBe(true);
    }
  });

  test('timeout fault via AbortSignal classifies as timeout keepStale', async () => {
    const harness = new BackendHarnessCore();
    harness.command('slow', { kind: 'fault', profile: 'timeout' });
    const controller = new AbortController();
    const pending = harness.handleInvoke('slow', undefined, controller.signal);
    controller.abort();
    let caught: unknown;
    try {
      await pending;
    } catch (error) {
      caught = error;
    }
    expect(caught).toBeDefined();
    const classification = classifyTransportFault(caught);
    expect(classification.kind).toBe('timeout');
    expect(classification.cachePolicy).toBe('keepStale');
    expect(classification.showRetry).toBe(true);
  });

  test('fetch networkOffline result classifies as offline', async () => {
    const harness = new BackendHarnessCore();
    harness.route('GET', '/api/health', { kind: 'fault', profile: 'networkOffline' });
    const result = await harness.handleFetch('GET', '/api/health');
    expect(result.ok).toBe(false);
    if (result.ok) {
      throw new Error('expected network fault');
    }
    const classification = classifyTransportFault({
      name: result.errorName,
      message: result.errorMessage,
      code: 'NETWORK_OFFLINE',
    });
    expect(classification.kind).toBe('networkOffline');
    expect(classification.cachePolicy).toBe('keepStale');
  });
});

describe('createTerminalFaultBridge', () => {
  test('disconnect zeroes listeners; reconnect restores exact baseline', () => {
    const bridge = createTerminalFaultBridge();
    const unlistenA = bridge.listen(() => undefined);
    const unlistenB = bridge.listen(() => undefined);
    expect(bridge.listenerCount).toBe(2);
    expect(bridge.connected).toBe(true);

    bridge.disconnect();
    expect(bridge.connected).toBe(false);
    expect(bridge.listenerCount).toBe(0);
    expect(bridge.baselineListenerCount).toBe(2);

    bridge.reconnect();
    expect(bridge.connected).toBe(true);
    expect(bridge.listenerCount).toBe(2);
    expect(bridge.listenerCount).toBe(bridge.baselineListenerCount);

    unlistenA();
    unlistenB();
    expect(bridge.listenerCount).toBe(0);
  });

  test('disconnect drops inputs; reconnect/replay does not double-count', () => {
    const bridge = createTerminalFaultBridge();
    bridge.listen(() => undefined);

    expect(bridge.acceptInput('i1', 'a')).toBe(true);
    expect(bridge.acceptInput('i2', 'b')).toBe(true);
    expect(bridge.inputCount).toBe(2);

    bridge.disconnect();
    expect(bridge.acceptInput('i3', 'c')).toBe(false);
    bridge.noteInput('i4', 'd');
    expect(bridge.inputCount).toBe(2);

    bridge.reconnect();
    // replay 旧 key 不重复写入
    expect(bridge.acceptInput('i1', 'a')).toBe(false);
    expect(bridge.acceptInput('i2', 'b')).toBe(false);
    expect(bridge.inputCount).toBe(2);
    // 新输入正常入账
    expect(bridge.acceptInput('i5', 'e')).toBe(true);
    expect(bridge.inputCount).toBe(3);
  });

  test('emit only delivers while connected', () => {
    const bridge = createTerminalFaultBridge();
    const received: unknown[] = [];
    bridge.listen((payload) => {
      received.push(payload);
    });

    bridge.emit({ n: 1 });
    expect(received).toEqual([{ n: 1 }]);

    bridge.disconnect();
    bridge.emit({ n: 2 });
    expect(received).toEqual([{ n: 1 }]);

    bridge.reconnect();
    bridge.emit({ n: 3 });
    expect(received).toEqual([{ n: 1 }, { n: 3 }]);
  });
});
