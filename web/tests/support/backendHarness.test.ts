/**
 * BackendHarness 纯核心合同测试（Vitest）。
 *
 * Business Logic（为什么需要这个套件）:
 *   L1 harness 是后续全部浏览器旅程的底座，必须先锁定 invoke/fetch/defer/fault/
 *   sequence/settlement 合同，避免 E2E 出现 unregistered 假阳性或泄漏假阴性。
 *
 * Code Logic（这个套件做什么）:
 *   直接驱动 BackendHarnessCore，不启动浏览器；覆盖成功、未注册、序列、defer、
 *   AbortSignal 超时、事件订阅与 assertSettled。
 */

import { describe, expect, test } from 'vitest';
import {
  BackendHarnessCore,
  HarnessSettlementError,
  HarnessUnregisteredError,
  matchPathTemplate,
  type HarnessBehavior,
} from './backendHarness';

/**
 * Business Logic（为什么需要这个函数）:
 *   多个用例需要快速拿到已注册 resolve 的 harness。
 *
 * Code Logic（这个函数做什么）:
 *   创建 core 并注册可选 command。
 */
function createCoreWithCommand(
  name: string,
  behavior: HarnessBehavior | HarnessBehavior[],
): BackendHarnessCore {
  const core = new BackendHarnessCore();
  core.command(name, behavior);
  return core;
}

describe('matchPathTemplate', () => {
  test('matches exact path', () => {
    expect(matchPathTemplate('/api/health', '/api/health')).toEqual({ params: {} });
  });

  test('matches :param segments and ignores query', () => {
    expect(matchPathTemplate('/api/transfer/status/:id', '/api/transfer/status/task-9?x=1')).toEqual({
      params: { id: 'task-9' },
    });
  });

  test('rejects different arity or literal mismatch', () => {
    expect(matchPathTemplate('/api/transfer/status/:id', '/api/transfer/status')).toBeNull();
    expect(matchPathTemplate('/api/a/:id', '/api/b/1')).toBeNull();
  });
});

describe('BackendHarnessCore invoke/fetch', () => {
  test('registered invoke resolve returns value and records call', async () => {
    const core = createCoreWithCommand('list_devices', {
      kind: 'resolve',
      value: [{ id: 'd1' }],
    });
    await expect(core.handleInvoke('list_devices', { limit: 1 })).resolves.toEqual([
      { id: 'd1' },
    ]);
    expect(core.calls()).toEqual([
      expect.objectContaining({
        type: 'invoke',
        command: 'list_devices',
        args: { limit: 1 },
      }),
    ]);
  });

  test('unregistered invoke fails with exact command name', async () => {
    const core = new BackendHarnessCore();
    await expect(core.handleInvoke('missing_command')).rejects.toSatisfy((error: unknown) => {
      return (
        error instanceof HarnessUnregisteredError &&
        error.surface === 'invoke' &&
        error.target === 'missing_command' &&
        error.message === 'Unregistered invoke: missing_command'
      );
    });
  });

  test('registered fetch resolve encodes JSON body', async () => {
    const core = new BackendHarnessCore();
    core.route('GET', '/api/mobile/attention', {
      kind: 'resolve',
      value: { items: [] },
    });
    const result = await core.handleFetch('GET', '/api/mobile/attention');
    expect(result).toEqual({
      ok: true,
      status: 200,
      statusText: 'OK',
      headers: { 'content-type': 'application/json' },
      bodyText: JSON.stringify({ items: [] }),
    });
    expect(core.calls()[0]).toEqual(
      expect.objectContaining({
        type: 'fetch',
        method: 'GET',
        path: '/api/mobile/attention',
      }),
    );
  });

  test('path template route matches and unregistered path includes method+path', async () => {
    const core = new BackendHarnessCore();
    core.route('GET', '/api/transfer/status/:id', {
      kind: 'resolve',
      value: { status: 'completed' },
    });
    const hit = await core.handleFetch('GET', '/api/transfer/status/abc');
    expect(hit.ok).toBe(true);
    if (hit.ok) {
      expect(JSON.parse(hit.bodyText)).toEqual({ status: 'completed' });
    }

    await expect(core.handleFetch('POST', '/api/unknown')).rejects.toSatisfy((error: unknown) => {
      return (
        error instanceof HarnessUnregisteredError &&
        error.message === 'Unregistered fetch: POST /api/unknown'
      );
    });
  });

  test('per-call sequence consumes then sticky is not assumed for arrays', async () => {
    const core = createCoreWithCommand('ping', [
      { kind: 'resolve', value: 1 },
      { kind: 'resolve', value: 2 },
    ]);
    await expect(core.handleInvoke('ping')).resolves.toBe(1);
    await expect(core.handleInvoke('ping')).resolves.toBe(2);
    await expect(core.handleInvoke('ping')).rejects.toThrow(
      'No remaining harness behavior for command ping',
    );
  });

  test('sticky single behavior can be reused', async () => {
    const core = createCoreWithCommand('version', { kind: 'resolve', value: '1.0.0' });
    await expect(core.handleInvoke('version')).resolves.toBe('1.0.0');
    await expect(core.handleInvoke('version')).resolves.toBe('1.0.0');
    core.assertSettled();
  });

  test('reject behavior throws provided error', async () => {
    const core = createCoreWithCommand('boom', {
      kind: 'reject',
      error: new Error('nope'),
    });
    await expect(core.handleInvoke('boom')).rejects.toThrow('nope');
  });
});

describe('BackendHarnessCore defer and AbortSignal', () => {
  test('deferred invoke resolves later and supports stale ordering', async () => {
    const core = new BackendHarnessCore();
    core.command('load_project', [
      { kind: 'defer', key: 'proj-a' },
      { kind: 'resolve', value: { id: 'b' } },
    ]);

    const first = core.handleInvoke('load_project');
    const second = await core.handleInvoke('load_project');
    expect(second).toEqual({ id: 'b' });

    let firstSettled = false;
    void first.then(() => {
      firstSettled = true;
    });
    await Promise.resolve();
    expect(firstSettled).toBe(false);

    core.resolveDeferred('proj-a', { id: 'a-late' });
    await expect(first).resolves.toEqual({ id: 'a-late' });
    core.assertSettled();
  });

  test('AbortSignal aborts deferred wait', async () => {
    const core = createCoreWithCommand('slow', { kind: 'defer', key: 'k1' });
    const controller = new AbortController();
    const pending = core.handleInvoke('slow', undefined, controller.signal);
    controller.abort();
    await expect(pending).rejects.toMatchObject({ name: 'AbortError' });
    // aborted defer should not leave pending
    core.assertSettled();
  });

  test('timeout fault rejects when AbortSignal aborts', async () => {
    const core = createCoreWithCommand('hang', { kind: 'fault', profile: 'timeout' });
    const controller = new AbortController();
    const pending = core.handleInvoke('hang', undefined, controller.signal);
    controller.abort();
    await expect(pending).rejects.toMatchObject({ name: 'AbortError' });
    core.assertSettled();
  });

  test('timeout fault without signal stays pending for assertSettled', async () => {
    const core = createCoreWithCommand('hang', { kind: 'fault', profile: 'timeout' });
    void core.handleInvoke('hang');
    await Promise.resolve();
    expect(() => core.assertSettled()).toThrow(HarnessSettlementError);
    expect(() => core.assertSettled()).toThrow(/pending requests/);
  });
});

describe('BackendHarnessCore faults', () => {
  test('permissionDenied and networkOffline produce coded errors', async () => {
    const core = new BackendHarnessCore();
    core.command('secure', { kind: 'fault', profile: 'permissionDenied' });
    core.route('GET', '/api/peer', { kind: 'fault', profile: 'networkOffline' });

    await expect(core.handleInvoke('secure')).rejects.toMatchObject({
      message: 'permission denied',
      code: 'PERMISSION_DENIED',
    });

    const fetchResult = await core.handleFetch('GET', '/api/peer');
    expect(fetchResult).toEqual({
      ok: false,
      errorName: 'TypeError',
      errorMessage: 'Failed to fetch',
    });
  });

  test('malformedJson fetch returns non-JSON body text', async () => {
    const core = new BackendHarnessCore();
    core.route('GET', '/api/broken', { kind: 'fault', profile: 'malformedJson' });
    const result = await core.handleFetch('GET', '/api/broken');
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.bodyText).toBe('{not-json');
      expect(() => JSON.parse(result.bodyText)).toThrow();
    }
  });
});

describe('BackendHarnessCore events and settlement', () => {
  test('subscribe emit and unsubscribe track listeners', () => {
    const core = new BackendHarnessCore();
    const payloads: unknown[] = [];
    const unlisten = core.subscribe('transfer:progress', (payload) => {
      payloads.push(payload);
    });
    core.emit('transfer:progress', { progress: 0.5 });
    expect(payloads).toEqual([{ progress: 0.5 }]);
    expect(() => core.assertSettled()).toThrow(/leaked event listeners/);
    unlisten();
    core.emit('transfer:progress', { progress: 1 });
    expect(payloads).toEqual([{ progress: 0.5 }]);
    core.assertSettled();
  });

  test('assertSettled detects unconsumed sequence expectations', async () => {
    const core = createCoreWithCommand('once', [
      { kind: 'resolve', value: 1 },
      { kind: 'resolve', value: 2 },
    ]);
    await core.handleInvoke('once');
    expect(() => core.assertSettled()).toThrow(/unconsumed command expectations for "once"/);
  });

  test('assertSettled detects pending deferred keys', () => {
    const core = createCoreWithCommand('late', { kind: 'defer', key: 'still-open' });
    void core.handleInvoke('late');
    expect(() => core.assertSettled()).toThrow(/pending deferred keys: still-open/);
    core.resolveDeferred('still-open', null);
    core.assertSettled();
  });

  test('plugin event listen/unlisten adjust listener count', async () => {
    const core = new BackendHarnessCore();
    const eventId = await core.handleInvoke('plugin:event|listen', {
      event: 'health:reminder',
      handler: 1,
    });
    expect(typeof eventId).toBe('number');
    expect(() => core.assertSettled()).toThrow(/leaked event listeners/);
    await core.handleInvoke('plugin:event|unlisten', {
      event: 'health:reminder',
      eventId,
    });
    core.assertSettled();
  });
});
