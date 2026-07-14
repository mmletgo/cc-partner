// @vitest-environment jsdom
/**
 * GUI 关闭前 pending write flush 门闩测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   关闭 GUI 或前后端都关闭时，若未 await 未落库正文，用户会静默丢数据。
 *
 * Code Logic（这个测试做什么）:
 *   1) 纯 helper：flushAll 必须在 stop/exit 之前；flush 失败不得 exit。
 *   2) BackendCloseChoiceListener：flush 拒绝时保持对话框并展示错误。
 */

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import { MemoryRouter } from 'react-router-dom';

import i18n from '@/i18n';
import { createPendingWriteRegistry } from '@/lib/pendingWrites';

import { BackendCloseChoiceListener } from './App';
import { flushPendingWritesThenClose } from '@/lib/closeFlush';

const exitGui = vi.fn();
const stop = vi.fn();

vi.mock('@/api/backend', () => ({
  backendApi: {
    exitGui: (...args: unknown[]) => exitGui(...args),
    stop: (...args: unknown[]) => stop(...args),
    status: vi.fn(),
    start: vi.fn(),
  },
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => undefined),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({
    label: 'main',
    onCloseRequested: vi.fn(async () => () => undefined),
  }),
}));

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要可控的慢 flush，验证 stop/exit 时序。
 *
 * Code Logic（这个函数做什么）:
 *   返回 promise 与 resolve/reject 控制器。
 */
function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T | PromiseLike<T>) => void;
  reject: (reason?: unknown) => void;
} {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

beforeEach(() => {
  exitGui.mockReset();
  stop.mockReset();
  exitGui.mockResolvedValue(undefined);
  stop.mockResolvedValue({ running: false });
  (window as Window & { __TAURI_INTERNALS__?: { transformCallback: unknown } }).__TAURI_INTERNALS__ = {
    transformCallback: () => undefined,
  };
});

afterEach(() => {
  cleanup();
  delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
});

describe('flushPendingWritesThenClose', () => {
  test('gui-only close awaits flushAll before exitGui and never stops backend', async () => {
    const order: string[] = [];
    const flushGate = deferred<void>();

    const run = flushPendingWritesThenClose('gui', {
      flushAll: async () => {
        order.push('flush');
        await flushGate.promise;
      },
      stop: async () => {
        order.push('stop');
        await stop();
      },
      exitGui: async () => {
        order.push('exit');
        await exitGui();
      },
    });

    await Promise.resolve();
    expect(order).toEqual(['flush']);
    expect(exitGui).not.toHaveBeenCalled();
    expect(stop).not.toHaveBeenCalled();

    flushGate.resolve();
    await run;

    expect(order).toEqual(['flush', 'exit']);
    expect(exitGui).toHaveBeenCalledTimes(1);
    expect(stop).not.toHaveBeenCalled();
  });

  test('full close awaits flushAll before stop and exitGui', async () => {
    const order: string[] = [];
    const flushGate = deferred<void>();

    const run = flushPendingWritesThenClose('full', {
      flushAll: async () => {
        order.push('flush');
        await flushGate.promise;
      },
      stop: async () => {
        order.push('stop');
        await stop();
      },
      exitGui: async () => {
        order.push('exit');
        await exitGui();
      },
    });

    await Promise.resolve();
    expect(order).toEqual(['flush']);
    expect(stop).not.toHaveBeenCalled();
    expect(exitGui).not.toHaveBeenCalled();

    flushGate.resolve();
    await run;

    expect(order).toEqual(['flush', 'stop', 'exit']);
    expect(stop).toHaveBeenCalledTimes(1);
    expect(exitGui).toHaveBeenCalledTimes(1);
  });

  test('flush rejection prevents stop and exit', async () => {
    await expect(
      flushPendingWritesThenClose('full', {
        flushAll: async () => {
          throw new Error('scratchpad save failed');
        },
        stop: async () => {
          await stop();
        },
        exitGui: async () => {
          await exitGui();
        },
      }),
    ).rejects.toThrow('scratchpad save failed');

    expect(stop).not.toHaveBeenCalled();
    expect(exitGui).not.toHaveBeenCalled();
  });
});

describe('BackendCloseChoiceListener flush gate', () => {
  test('flush rejection keeps dialog open, resets busy, and shows close-dialog error', async () => {
    const registry = createPendingWriteRegistry();
    registry.register('scratchpad-autosave', async () => {
      throw new Error('pending scratchpad write failed');
    });

    const pendingWritesMod = await import('@/lib/pendingWrites');
    const flushAllSpy = vi
      .spyOn(pendingWritesMod.pendingWrites, 'flushAll')
      .mockImplementation(() => registry.flushAll());

    render(
      <MemoryRouter>
        <I18nextProvider i18n={i18n}>
          <BackendCloseChoiceListener initialOpenForTest />
        </I18nextProvider>
      </MemoryRouter>,
    );

    expect(screen.getByRole('dialog')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '仅关闭 GUI' }));

    await waitFor(() => {
      expect(screen.getByText(/pending scratchpad write failed/)).toBeTruthy();
    });

    expect(screen.getByRole('dialog')).toBeTruthy();
    expect(exitGui).not.toHaveBeenCalled();
    expect(stop).not.toHaveBeenCalled();

    expect((screen.getByRole('button', { name: '仅关闭 GUI' }) as HTMLButtonElement).disabled).toBe(false);
    expect((screen.getByRole('button', { name: '前后端都关闭' }) as HTMLButtonElement).disabled).toBe(false);

    flushAllSpy.mockRestore();
  });

  test('successful flush then full close calls stop and exit in order', async () => {
    const order: string[] = [];
    const pendingWritesMod = await import('@/lib/pendingWrites');
    const flushAllSpy = vi.spyOn(pendingWritesMod.pendingWrites, 'flushAll').mockImplementation(async () => {
      order.push('flush');
    });
    stop.mockImplementation(async () => {
      order.push('stop');
      return { running: false };
    });
    exitGui.mockImplementation(async () => {
      order.push('exit');
    });

    render(
      <MemoryRouter>
        <I18nextProvider i18n={i18n}>
          <BackendCloseChoiceListener initialOpenForTest />
        </I18nextProvider>
      </MemoryRouter>,
    );

    fireEvent.click(screen.getByRole('button', { name: '前后端都关闭' }));

    await waitFor(() => {
      expect(order).toEqual(['flush', 'stop', 'exit']);
    });

    flushAllSpy.mockRestore();
  });
});
