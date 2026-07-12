import { describe, test } from 'vitest';
import {
  OrchestratorRuntimeTransportError,
  isOrchestratorRuntimeNetworkTransportError,
  toOrchestratorRuntimeTransportError,
  toRuntimeLoadError,
} from './orchestratorRuntimeTransportError';

/**
 * Business Logic（为什么需要这个函数）:
 *   传输错误 helper 测试需要明确失败原因，避免 silent pass。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛出带消息的 Error。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

describe('orchestratorRuntimeTransportError', () => {
  test('toRuntimeLoadError preserves transport kind and only wraps non-Error rejects', () => {
    const network = new OrchestratorRuntimeTransportError('fetch failed', 'network');
    const preserved = toRuntimeLoadError(network);
    assert(preserved === network, 'must pass OrchestratorRuntimeTransportError through');
    assert(
      isOrchestratorRuntimeNetworkTransportError(preserved),
      'network kind must survive panel catch path',
    );

    const plain = new Error('plain boom');
    const plainOut = toRuntimeLoadError(plain);
    assert(plainOut === plain, 'plain Error must pass through without rewrap');
    assert(
      !isOrchestratorRuntimeNetworkTransportError(plainOut),
      'plain Error is not network transport',
    );

    const wrapped = toRuntimeLoadError('string reject');
    assert(
      wrapped instanceof OrchestratorRuntimeTransportError,
      'non-Error reject becomes transport error',
    );
    assert(
      (wrapped as OrchestratorRuntimeTransportError).kind === 'unknown',
      'non-Error reject defaults to unknown kind',
    );
    assert(wrapped.message === 'string reject', 'string reject message preserved');

    const fromAdapter = toOrchestratorRuntimeTransportError(new TypeError('Failed to fetch'), 'network');
    assert(fromAdapter.kind === 'network', 'adapter helper still stamps network');
    assert(
      toRuntimeLoadError(fromAdapter) === fromAdapter,
      'adapter network error survives toRuntimeLoadError',
    );
  });
});
