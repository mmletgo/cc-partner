// @vitest-environment jsdom
/**
 * Scratchpad 生命周期与 autosave 集成测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   切页、路由卸载与保存失败重试必须保证正文不丢；否则用户会以为内容已保存。
 *
 * Code Logic（这个测试做什么）:
 *   注入可观察的 ScratchpadAutosaveQueue + mock scratchpadApi，覆盖：
 *   输入后未满 debounce 即卸载会 flush 启动保存、切页先 flush 再 get、
 *   失败项保留并可重试。
 */

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import type { ReactNode } from 'react';

import i18n from '@/i18n';
import type { ScratchpadPage, ScratchpadPageSummary } from '@/lib/types';
import {
  createScratchpadAutosaveQueue,
  type ScratchpadAutosaveQueue,
  type ScratchpadAutosaveSaveFn,
} from '@/hooks/scratchpadAutosave';
import { ScratchpadAutosaveContext } from '@/hooks/scratchpadAutosaveContext';

import { Scratchpad } from './Scratchpad';

const listPages = vi.fn();
const getPage = vi.fn();
const createPage = vi.fn();
const updatePageContent = vi.fn();
const renamePage = vi.fn();
const deletePage = vi.fn();
const sync = vi.fn();
const triggerCloudSync = vi.fn();

vi.mock('@/api/scratchpad', () => ({
  scratchpadApi: {
    listPages: (...args: unknown[]) => listPages(...args),
    getPage: (...args: unknown[]) => getPage(...args),
    createPage: (...args: unknown[]) => createPage(...args),
    updatePageContent: (...args: unknown[]) => updatePageContent(...args),
    renamePage: (...args: unknown[]) => renamePage(...args),
    deletePage: (...args: unknown[]) => deletePage(...args),
    sync: (...args: unknown[]) => sync(...args),
  },
}));

vi.mock('@/api/config', () => ({
  configApi: {
    triggerCloudSync: (...args: unknown[]) => triggerCloudSync(...args),
  },
}));

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要可控的慢/失败保存，以验证 flush 时序。
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

/**
 * Business Logic（为什么需要这个函数）:
 *   用例需要最小合法页面 DTO。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的 ScratchpadPage。
 */
function buildPage(overrides: Partial<ScratchpadPage> = {}): ScratchpadPage {
  return {
    id: 'page-1',
    title: '页面一',
    content: '初始内容',
    createdAt: '2026-07-13T10:00:00.000Z',
    updatedAt: '2026-07-13T10:00:00.000Z',
    deviceId: 'device-a',
    vectorClock: { 'device-a': 1 },
    deleted: false,
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   列表摘要与完整页共享基础字段。
 *
 * Code Logic（这个函数做什么）:
 *   从完整页投影出 ScratchpadPageSummary。
 */
function toSummary(page: ScratchpadPage): ScratchpadPageSummary {
  return {
    id: page.id,
    title: page.title,
    updatedAt: page.updatedAt,
    deviceId: page.deviceId,
    deleted: page.deleted,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   页面必须在 Provider 内才能访问常驻队列。
 *
 * Code Logic（这个函数做什么）:
 *   用注入的 queue 包裹 Scratchpad + i18n。
 */
function renderScratchpad(queue: ScratchpadAutosaveQueue) {
  /**
   * Business Logic（为什么需要这个组件）:
   *   测试注入可观察 queue，不依赖生产 Provider 的真实 API save。
   *
   * Code Logic（这个组件做什么）:
   *   把 queue 写入 ScratchpadAutosaveContext。
   */
  function Wrapper({ children }: { children: ReactNode }) {
    return (
      <I18nextProvider i18n={i18n}>
        <ScratchpadAutosaveContext.Provider value={queue}>{children}</ScratchpadAutosaveContext.Provider>
      </I18nextProvider>
    );
  }

  return render(<Scratchpad />, { wrapper: Wrapper });
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  listPages.mockReset();
  getPage.mockReset();
  createPage.mockReset();
  updatePageContent.mockReset();
  renamePage.mockReset();
  deletePage.mockReset();
  sync.mockReset();
  triggerCloudSync.mockReset();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe('Scratchpad autosave lifecycle', () => {
  test('typing then unmount before debounce starts save via flushAll', async () => {
    const page = buildPage({ content: '' });
    listPages.mockResolvedValue([toSummary(page)]);
    getPage.mockResolvedValue(page);

    const save = vi.fn<ScratchpadAutosaveSaveFn>(async () => undefined);
    const queue = createScratchpadAutosaveQueue(save, { delayMs: 500 });
    const flushAllSpy = vi.spyOn(queue, 'flushAll');

    const view = renderScratchpad(queue);

    await waitFor(() => {
      expect((screen.getByLabelText('速记本内容') as HTMLTextAreaElement).disabled).toBe(false);
    });

    const editor = screen.getByLabelText('速记本内容');
    fireEvent.change(editor, { target: { value: '未满 debounce 就离开' } });

    expect(save).not.toHaveBeenCalled();

    await act(async () => {
      view.unmount();
    });

    expect(flushAllSpy).toHaveBeenCalled();
    await waitFor(() => {
      expect(save).toHaveBeenCalledWith('page-1', '未满 debounce 就离开');
    });
  });

  test('switching page awaits flushPage before getPage for the target', async () => {
    const page1 = buildPage({ id: 'page-1', title: '页面一', content: 'a' });
    const page2 = buildPage({ id: 'page-2', title: '页面二', content: 'b' });
    listPages.mockResolvedValue([toSummary(page1), toSummary(page2)]);
    getPage.mockImplementation(async (pageId: string) => {
      if (pageId === 'page-1') return page1;
      if (pageId === 'page-2') return page2;
      throw new Error(`unknown page ${pageId}`);
    });

    const order: string[] = [];
    const saveGate = deferred<void>();
    const save = vi.fn<ScratchpadAutosaveSaveFn>(async (pageId, content) => {
      order.push(`save:${pageId}:${content}`);
      await saveGate.promise;
    });
    const queue = createScratchpadAutosaveQueue(save, { delayMs: 500 });

    renderScratchpad(queue);

    await waitFor(() => {
      expect((screen.getByLabelText('速记本内容') as HTMLTextAreaElement).disabled).toBe(false);
    });

    getPage.mockClear();
    getPage.mockImplementation(async (pageId: string) => {
      order.push(`get:${pageId}`);
      if (pageId === 'page-1') return page1;
      if (pageId === 'page-2') return { ...page2, content: 'b-fresh' };
      throw new Error(`unknown page ${pageId}`);
    });

    fireEvent.change(screen.getByLabelText('速记本内容'), {
      target: { value: 'page1-edited' },
    });

    fireEvent.click(screen.getByRole('button', { name: /页面二/ }));

    // flush 尚未完成时不得 get 目标页
    await act(async () => {
      await Promise.resolve();
    });
    expect(order.some((entry) => entry.startsWith('get:page-2'))).toBe(false);
    expect(order.some((entry) => entry.startsWith('save:page-1'))).toBe(true);

    saveGate.resolve();
    await waitFor(() => {
      expect(order).toContain('get:page-2');
    });

    const firstSaveIdx = order.findIndex((entry) => entry.startsWith('save:page-1'));
    const getIdx = order.findIndex((entry) => entry === 'get:page-2');
    expect(firstSaveIdx).toBeGreaterThanOrEqual(0);
    expect(getIdx).toBeGreaterThan(firstSaveIdx);

    await waitFor(() => {
      expect((screen.getByLabelText('速记本内容') as HTMLTextAreaElement).value).toBe('b-fresh');
    });
  });

  test('failed queue item remains retryable in the UI', async () => {
    const page = buildPage({ content: '' });
    listPages.mockResolvedValue([toSummary(page)]);
    getPage.mockResolvedValue(page);

    let call = 0;
    const save = vi.fn<ScratchpadAutosaveSaveFn>(async () => {
      call += 1;
      if (call === 1) {
        throw new Error('disk full');
      }
    });
    const queue = createScratchpadAutosaveQueue(save, { delayMs: 500 });

    renderScratchpad(queue);

    await waitFor(() => {
      expect((screen.getByLabelText('速记本内容') as HTMLTextAreaElement).disabled).toBe(false);
    });

    fireEvent.change(screen.getByLabelText('速记本内容'), {
      target: { value: '需要重试' },
    });

    await act(async () => {
      await queue.flushPage('page-1').catch(() => undefined);
    });

    await waitFor(() => {
      expect(screen.getByText(/disk full|速记本保存失败/)).toBeTruthy();
      expect(screen.getByRole('button', { name: '重试保存' })).toBeTruthy();
    });

    const snapBefore = queue.getSnapshot().pages['page-1'];
    expect(snapBefore?.pendingVersion).toBeGreaterThan(snapBefore?.savedVersion ?? 0);
    expect(snapBefore?.content).toBe('需要重试');

    fireEvent.click(screen.getByRole('button', { name: '重试保存' }));

    await waitFor(() => {
      expect(save).toHaveBeenCalledTimes(2);
      expect(queue.getSnapshot().pages['page-1']?.error).toBeNull();
      expect(queue.getSnapshot().pages['page-1']?.savedVersion).toBe(
        queue.getSnapshot().pages['page-1']?.pendingVersion,
      );
    });
  });
});
