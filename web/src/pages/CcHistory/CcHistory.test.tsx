// @vitest-environment jsdom
/**
 * CcHistory 页面逆序响应与失败反馈契约测试
 *
 * Business Logic（为什么需要这个测试）:
 *   用户切换项目 A→B、搜索 a→ab，或刷新/同步失败时，UI 必须只反映最新上下文，
 *   旧响应不得覆盖 prompts/error/loading；刷新与同步失败须有非阻塞提示。
 *
 * Code Logic（这个测试做什么）:
 *   mock ccHistoryApi / promptsApi，用 deferred Promise 制造逆序 resolve；
 *   断言 DOM 中仅最新项目/搜索词的 prompt 文案可见，以及 refresh/sync 失败 toast。
 */

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import type { CcHistoryItem, CcProject } from '@/lib/types';
import { CcHistory } from './CcHistory';

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason?: unknown) => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   逆序响应测试需要手动控制 resolve 时机。
 *
 * Code Logic（这个函数做什么）:
 *   返回 promise 与 resolve/reject 控制器。
 */
function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const fakeCcHistoryApi = vi.hoisted(() => ({
  listProjects: vi.fn(),
  listPrompts: vi.fn(),
  refresh: vi.fn(),
  remove: vi.fn(),
}));

const fakePromptsApi = vi.hoisted(() => ({
  sync: vi.fn(),
  create: vi.fn(),
  list: vi.fn(),
  get: vi.fn(),
  update: vi.fn(),
  remove: vi.fn(),
  listTags: vi.fn(),
}));

vi.mock('@/api/ccHistory', () => ({
  ccHistoryApi: fakeCcHistoryApi,
}));

vi.mock('@/api/prompts', () => ({
  promptsApi: fakePromptsApi,
}));

/**
 * Business Logic（为什么需要这个函数）:
 *   用例需要可区分的项目 fixture。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的 CcProject。
 */
function buildProject(overrides: Partial<CcProject> = {}): CcProject {
  return {
    projectPath: '/projects/A',
    projectName: 'ProjectA',
    count: 1,
    lastOccurredAt: '2026-07-13T10:00:00.000Z',
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用例需要可区分的 prompt 文案以断言 DOM。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的 CcHistoryItem。
 */
function buildPrompt(overrides: Partial<CcHistoryItem> = {}): CcHistoryItem {
  return {
    id: 'p-1',
    projectPath: '/projects/A',
    projectName: 'ProjectA',
    sessionId: 's-1',
    content: 'prompt-A',
    occurredAt: '2026-07-13T10:00:00.000Z',
    deviceId: 'dev-1',
    createdAt: '2026-07-13T10:00:00.000Z',
    deleted: false,
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   契约测试需要统一挂载 i18n。
 *
 * Code Logic（这个函数做什么）:
 *   用 I18nextProvider 渲染 CcHistory。
 */
function renderPage() {
  return render(
    <I18nextProvider i18n={i18n}>
      <CcHistory />
    </I18nextProvider>,
  );
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

beforeEach(() => {
  fakeCcHistoryApi.listProjects.mockReset();
  fakeCcHistoryApi.listPrompts.mockReset();
  fakeCcHistoryApi.refresh.mockReset();
  fakeCcHistoryApi.remove.mockReset();
  fakePromptsApi.sync.mockReset();
  fakePromptsApi.create.mockReset();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('CcHistory stale response guards', () => {
  test('可从项目筛选器直接选择项目并加载对应 Prompt', async () => {
    const projectA = buildProject({
      projectPath: '/projects/A',
      projectName: 'ProjectA',
    });
    const projectB = buildProject({
      projectPath: '/projects/B',
      projectName: 'ProjectB',
    });
    fakeCcHistoryApi.listProjects.mockResolvedValue([projectA, projectB]);
    fakeCcHistoryApi.listPrompts.mockImplementation(async (projectPath: string) => [
      buildPrompt({
        id: `prompt-${projectPath}`,
        projectPath,
        projectName: projectPath === '/projects/A' ? 'ProjectA' : 'ProjectB',
        content: projectPath === '/projects/A' ? 'PROJECT-A-PROMPT' : 'PROJECT-B-PROMPT',
      }),
    ]);

    renderPage();

    const projectFilter = await screen.findByRole('combobox', {
      name: '按项目筛选 Claude 历史',
    });
    expect((projectFilter as HTMLSelectElement).value).toBe('/projects/A');
    expect(screen.getByRole('option', { name: 'ProjectB · /projects/B' })).toBeTruthy();

    fireEvent.change(projectFilter, { target: { value: '/projects/B' } });

    await waitFor(() => {
      expect(fakeCcHistoryApi.listPrompts).toHaveBeenCalledWith('/projects/B', undefined);
      expect(screen.getByText('PROJECT-B-PROMPT')).toBeTruthy();
    });
  });

  test('项目 A→B 时逆序 resolve 只保留 B 的 prompts', async () => {
    const projectA = buildProject({
      projectPath: '/projects/A',
      projectName: 'ProjectA',
      count: 2,
    });
    const projectB = buildProject({
      projectPath: '/projects/B',
      projectName: 'ProjectB',
      count: 2,
    });

    fakeCcHistoryApi.listProjects.mockResolvedValue([projectA, projectB]);

    const promptsA = deferred<CcHistoryItem[]>();
    const promptsB = deferred<CcHistoryItem[]>();
    let aCalls = 0;
    let bCalls = 0;

    fakeCcHistoryApi.listPrompts.mockImplementation(async (projectPath: string) => {
      if (projectPath === '/projects/A') {
        aCalls += 1;
        return promptsA.promise;
      }
      if (projectPath === '/projects/B') {
        bCalls += 1;
        return promptsB.promise;
      }
      return [];
    });

    renderPage();

    await waitFor(() => {
      expect(screen.getByText('ProjectA')).toBeTruthy();
      expect(screen.getByText('ProjectB')).toBeTruthy();
    });
    // 默认选中 A 后 effect 会发起 listPrompts(A)
    await waitFor(() => {
      expect(aCalls).toBeGreaterThanOrEqual(1);
    });

    fireEvent.click(screen.getByRole('button', { name: /ProjectB/i }));
    await waitFor(() => {
      expect(bCalls).toBeGreaterThanOrEqual(1);
    });

    await act(async () => {
      promptsB.resolve([
        buildPrompt({
          id: 'b-1',
          projectPath: '/projects/B',
          projectName: 'ProjectB',
          content: 'ONLY-B-PROMPT',
        }),
      ]);
    });

    await waitFor(() => {
      expect(screen.getByText('ONLY-B-PROMPT')).toBeTruthy();
    });

    await act(async () => {
      promptsA.resolve([
        buildPrompt({
          id: 'a-1',
          projectPath: '/projects/A',
          projectName: 'ProjectA',
          content: 'STALE-A-PROMPT',
        }),
      ]);
      await Promise.resolve();
    });

    expect(screen.getByText('ONLY-B-PROMPT')).toBeTruthy();
    expect(screen.queryByText('STALE-A-PROMPT')).toBeNull();
  });

  test('搜索 a→ab 时逆序 resolve 只保留 ab 的 prompts', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });

    const projectA = buildProject({
      projectPath: '/projects/A',
      projectName: 'ProjectA',
      count: 3,
    });
    fakeCcHistoryApi.listProjects.mockResolvedValue([projectA]);

    const bySearch = new Map<string, Deferred<CcHistoryItem[]>>();

    fakeCcHistoryApi.listPrompts.mockImplementation(
      async (projectPath: string, search?: string) => {
        expect(projectPath).toBe('/projects/A');
        const key = search ?? '';
        const existing = bySearch.get(key);
        if (existing) return existing.promise;
        const d = deferred<CcHistoryItem[]>();
        bySearch.set(key, d);
        return d.promise;
      },
    );

    renderPage();

    await waitFor(() => {
      expect(bySearch.has('')).toBe(true);
    });
    await act(async () => {
      bySearch.get('')!.resolve([
        buildPrompt({ id: 'base', content: 'BASE-PROMPT' }),
      ]);
    });
    await waitFor(() => {
      expect(screen.getByText('BASE-PROMPT')).toBeTruthy();
    });

    const searchInput = screen.getByLabelText('搜索 Claude 历史 Prompt');
    fireEvent.change(searchInput, { target: { value: 'a' } });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(300);
    });
    await waitFor(() => {
      expect(bySearch.has('a')).toBe(true);
    });

    fireEvent.change(searchInput, { target: { value: 'ab' } });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(300);
    });
    await waitFor(() => {
      expect(bySearch.has('ab')).toBe(true);
    });

    await act(async () => {
      bySearch.get('ab')!.resolve([
        buildPrompt({ id: 'ab-1', content: 'MATCH-AB-PROMPT' }),
      ]);
    });
    await waitFor(() => {
      expect(screen.getByText('MATCH-AB-PROMPT')).toBeTruthy();
    });

    await act(async () => {
      bySearch.get('a')!.resolve([
        buildPrompt({ id: 'a-1', content: 'STALE-A-SEARCH-PROMPT' }),
      ]);
      await Promise.resolve();
    });

    expect(screen.getByText('MATCH-AB-PROMPT')).toBeTruthy();
    expect(screen.queryByText('STALE-A-SEARCH-PROMPT')).toBeNull();
  });

  test('旧项目请求失败不得覆盖新项目成功结果', async () => {
    const projectA = buildProject({
      projectPath: '/projects/A',
      projectName: 'ProjectA',
    });
    const projectB = buildProject({
      projectPath: '/projects/B',
      projectName: 'ProjectB',
    });
    fakeCcHistoryApi.listProjects.mockResolvedValue([projectA, projectB]);

    const promptsA = deferred<CcHistoryItem[]>();
    fakeCcHistoryApi.listPrompts.mockImplementation(async (projectPath: string) => {
      if (projectPath === '/projects/A') return promptsA.promise;
      return [
        buildPrompt({
          id: 'b-ok',
          projectPath: '/projects/B',
          projectName: 'ProjectB',
          content: 'B-OK-PROMPT',
        }),
      ];
    });

    renderPage();
    await waitFor(() => {
      expect(screen.getByText('ProjectB')).toBeTruthy();
    });

    fireEvent.click(screen.getByRole('button', { name: /ProjectB/i }));
    await waitFor(() => {
      expect(screen.getByText('B-OK-PROMPT')).toBeTruthy();
    });

    await act(async () => {
      promptsA.reject(new Error('stale-A-failed'));
      await Promise.resolve();
    });

    expect(screen.getByText('B-OK-PROMPT')).toBeTruthy();
    expect(screen.queryByText(/stale-A-failed/)).toBeNull();
  });

  test('刷新采集失败显示 toast，不静默', async () => {
    fakeCcHistoryApi.listProjects.mockResolvedValue([]);
    fakeCcHistoryApi.listPrompts.mockResolvedValue([]);
    fakeCcHistoryApi.refresh.mockRejectedValue(new Error('refresh-boom'));

    renderPage();
    await waitFor(() => {
      expect(screen.getByRole('button', { name: '刷新采集' })).toBeTruthy();
    });

    fireEvent.click(screen.getByRole('button', { name: '刷新采集' }));

    await waitFor(() => {
      expect(screen.getByText(/刷新采集失败/)).toBeTruthy();
      expect(screen.getByText(/refresh-boom/)).toBeTruthy();
    });
  });

  test('同步失败显示 toast，不静默', async () => {
    fakeCcHistoryApi.listProjects.mockResolvedValue([]);
    fakeCcHistoryApi.listPrompts.mockResolvedValue([]);
    fakePromptsApi.sync.mockRejectedValue(new Error('sync-boom'));

    renderPage();
    await waitFor(() => {
      expect(screen.getByRole('button', { name: '同步' })).toBeTruthy();
    });

    fireEvent.click(screen.getByRole('button', { name: '同步' }));

    await waitFor(() => {
      expect(screen.getByText(/同步失败/)).toBeTruthy();
      expect(screen.getByText(/sync-boom/)).toBeTruthy();
    });
  });
});

describe('CcHistory action failure recovery', () => {
  /**
   * Business Logic（为什么需要这个函数）:
   *   动作失败合同需要一条已加载的历史 Prompt 作为操作对象。
   *
   * Code Logic（这个函数做什么）:
   *   mock 单项目 + 单 prompt 并等待卡片渲染完成。
   */
  async function renderWithOnePrompt(
    item: CcHistoryItem = buildPrompt({ id: 'keep-1', content: 'KEEP-ME-PROMPT' }),
  ): Promise<CcHistoryItem> {
    const project = buildProject({
      projectPath: item.projectPath,
      projectName: item.projectName,
      count: 1,
    });
    fakeCcHistoryApi.listProjects.mockResolvedValue([project]);
    fakeCcHistoryApi.listPrompts.mockResolvedValue([item]);
    renderPage();
    await waitFor(() => {
      expect(screen.getByText(item.content)).toBeTruthy();
    });
    return item;
  }

  test('保存到 Prompt 失败显示 alert 与可重试，不静默', async () => {
    const item = await renderWithOnePrompt(
      buildPrompt({ id: 'save-fail', content: 'SAVE-FAIL-PROMPT' }),
    );
    fakePromptsApi.create.mockRejectedValue(new Error('prompt-create-down'));

    fireEvent.click(screen.getByRole('button', { name: '转存为 Prompt' }));

    await waitFor(() => {
      expect(fakePromptsApi.create).toHaveBeenCalled();
      expect(screen.getByRole('alert')).toBeTruthy();
      expect(screen.getByText(/转存到 Prompt 失败|保存到 Prompt/)).toBeTruthy();
      expect(screen.getByRole('button', { name: '重试' })).toBeTruthy();
    });

    // 条目本身仍在
    expect(screen.getByText(item.content)).toBeTruthy();

    fakePromptsApi.create.mockResolvedValue({ id: 'new-prompt' });
    fireEvent.click(screen.getByRole('button', { name: '重试' }));

    await waitFor(() => {
      expect(fakePromptsApi.create).toHaveBeenCalledTimes(2);
      expect(screen.getByText(/已转存到 Prompt 库/)).toBeTruthy();
    });
  });

  test('删除失败回滚列表项并提供重试', async () => {
    const item = await renderWithOnePrompt(
      buildPrompt({ id: 'del-fail', content: 'DELETE-FAIL-PROMPT' }),
    );
    fakeCcHistoryApi.remove.mockRejectedValue(new Error('delete-offline'));

    fireEvent.click(screen.getByRole('button', { name: '删除' }));
    await waitFor(() => {
      expect(screen.getByRole('button', { name: '删除' })).toBeTruthy();
      expect(screen.getByText(/确认要从历史中删除/)).toBeTruthy();
    });

    // 确认弹层中的危险删除按钮
    const confirmButtons = screen.getAllByRole('button', { name: '删除' });
    fireEvent.click(confirmButtons[confirmButtons.length - 1]);

    await waitFor(() => {
      expect(fakeCcHistoryApi.remove).toHaveBeenCalledWith('del-fail');
    });

    // 失败后条目回滚可见
    await waitFor(() => {
      expect(screen.getByText(item.content)).toBeTruthy();
      expect(screen.getByRole('alert')).toBeTruthy();
      expect(screen.getByText(/删除失败/)).toBeTruthy();
      expect(screen.getByRole('button', { name: '重试' })).toBeTruthy();
    });

    // 重试再次调用 remove
    fakeCcHistoryApi.remove.mockResolvedValue({ ok: true });
    fireEvent.click(screen.getByRole('button', { name: '重试' }));

    await waitFor(() => {
      expect(fakeCcHistoryApi.remove).toHaveBeenCalledTimes(2);
      expect(screen.queryByText(item.content)).toBeNull();
    });
  });

  test('成功删除不暴露假 Undo', async () => {
    const item = await renderWithOnePrompt(
      buildPrompt({ id: 'del-ok', content: 'DELETE-OK-PROMPT' }),
    );
    fakeCcHistoryApi.remove.mockResolvedValue({ ok: true });

    fireEvent.click(screen.getByRole('button', { name: '删除' }));
    await waitFor(() => {
      expect(screen.getByText(/确认要从历史中删除/)).toBeTruthy();
    });
    const confirmButtons = screen.getAllByRole('button', { name: '删除' });
    fireEvent.click(confirmButtons[confirmButtons.length - 1]);

    await waitFor(() => {
      expect(fakeCcHistoryApi.remove).toHaveBeenCalledWith('del-ok');
      expect(screen.queryByText(item.content)).toBeNull();
    });

    expect(screen.queryByRole('button', { name: /撤销|Undo/i })).toBeNull();
    expect(screen.queryByText(/撤销|Undo/i)).toBeNull();
  });
});
