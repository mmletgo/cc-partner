// @vitest-environment jsdom
/**
 * Prompts 页面 mutation 回滚 / 重试契约测试
 *
 * Business Logic（为什么需要这个测试）:
 *   create/update/delete/sync 失败不得静默保留乐观状态；用户必须看到错误并能用原 payload 重试。
 *
 * Code Logic（这个测试做什么）:
 *   mock promptsApi，渲染 Prompts，断言失败回滚、草稿恢复、错误可见、重试 payload 与 pending 禁用。
 */

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import type { ContentVersion, Prompt } from '@/lib/types';
import { promptsApi } from '@/api/prompts';
import { Prompts } from './Prompts';

vi.mock('@/api/prompts', () => ({
  promptsApi: {
    list: vi.fn(),
    listTags: vi.fn(),
    create: vi.fn(),
    update: vi.fn(),
    remove: vi.fn(),
    sync: vi.fn(),
    get: vi.fn(),
    listVersions: vi.fn(),
    restoreVersion: vi.fn(),
  },
}));

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要稳定的 Prompt 行夹具。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的最小合法 Prompt。
 */
function buildPrompt(overrides: Partial<Prompt> = {}): Prompt {
  return {
    id: 'p-1',
    title: 'Alpha title',
    content: 'Alpha content body',
    tags: ['work'],
    favorite: false,
    updatedAt: '2026-07-13T10:00:00.000Z',
    ...overrides,
  };
}

const initialPrompts: Prompt[] = [
  buildPrompt({ id: 'p-1', title: 'Alpha title', content: 'Alpha content body', tags: ['work'] }),
  buildPrompt({ id: 'p-2', title: 'Beta title', content: 'Beta content body', tags: ['life'] }),
];

/**
 * Business Logic（为什么需要这个函数）:
 *   契约测试需要统一挂载 i18n。
 *
 * Code Logic（这个函数做什么）:
 *   用 I18nextProvider 渲染 Prompts。
 */
function renderPrompts() {
  return render(
    <I18nextProvider i18n={i18n}>
      <Prompts />
    </I18nextProvider>,
  );
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

/**
 * Business Logic（为什么需要这个函数）:
 *   版本历史契约需要稳定 ContentVersion 夹具。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的最小合法 ContentVersion。
 */
function buildVersion(overrides: Partial<ContentVersion> = {}): ContentVersion {
  return {
    id: 'v-1',
    sourceDevice: 'device-b',
    contentHash: 'hash-1',
    createdAt: '2026-07-13T11:00:00.000Z',
    kind: 'history',
    title: 'Alpha title',
    contentPreview: 'older preview',
    content: 'older full content',
    ...overrides,
  };
}

beforeEach(() => {
  vi.mocked(promptsApi.list).mockResolvedValue(initialPrompts);
  vi.mocked(promptsApi.listTags).mockResolvedValue(['work', 'life']);
  vi.mocked(promptsApi.create).mockReset();
  vi.mocked(promptsApi.update).mockReset();
  vi.mocked(promptsApi.remove).mockReset();
  vi.mocked(promptsApi.sync).mockReset();
  vi.mocked(promptsApi.listVersions).mockReset();
  vi.mocked(promptsApi.restoreVersion).mockReset();
  vi.mocked(promptsApi.listVersions).mockResolvedValue([]);
  vi.stubGlobal('confirm', vi.fn(() => true));
  Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    value: {
      writeText: vi.fn().mockResolvedValue(undefined),
    },
  });
});

afterEach(() => {
  cleanup();
});

describe('Prompts mutation UI contracts', () => {
  test('double-click create invokes create API once', async () => {
    let resolveCreate: ((value: Prompt) => void) | undefined;
    vi.mocked(promptsApi.create).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveCreate = resolve;
        }),
    );

    renderPrompts();
    await screen.findByText('Alpha title');

    fireEvent.click(screen.getByRole('button', { name: /新建|New/i }));
    const titleInput = await screen.findByLabelText(/Prompt 标题|Prompt title/i);
    const contentInput = screen.getByLabelText(/Prompt 内容|Prompt content/i);
    fireEvent.change(titleInput, { target: { value: 'Once only' } });
    fireEvent.change(contentInput, { target: { value: 'Single submit body' } });

    const saveBtn = screen.getByRole('button', { name: /保存|Save/i });
    fireEvent.click(saveBtn);
    // 第二次点击在 re-render 前同步发生；pending ref 门闩应挡住第二次 create
    fireEvent.click(saveBtn);

    await waitFor(() => {
      expect(promptsApi.create).toHaveBeenCalledTimes(1);
    });
    expect(promptsApi.create).toHaveBeenCalledWith({
      title: 'Once only',
      content: 'Single submit body',
      tags: [],
    });

    await act(async () => {
      resolveCreate?.(
        buildPrompt({
          id: 'server-once',
          title: 'Once only',
          content: 'Single submit body',
          tags: [],
        }),
      );
    });

    await waitFor(() => {
      expect(screen.getByText('Once only')).toBeTruthy();
    });
  });

  test('create rejection rolls back row, reopens draft, shows error, and retry reuses payload', async () => {
    vi.mocked(promptsApi.create)
      .mockRejectedValueOnce(new Error('create-denied'))
      .mockResolvedValueOnce(
        buildPrompt({
          id: 'server-created',
          title: 'Brand new',
          content: 'Fresh body',
          tags: ['work'],
        }),
      );

    renderPrompts();
    await screen.findByText('Alpha title');

    fireEvent.click(screen.getByRole('button', { name: /新建|New/i }));
    const titleInput = await screen.findByLabelText(/Prompt 标题|Prompt title/i);
    const contentInput = screen.getByLabelText(/Prompt 内容|Prompt content/i);
    fireEvent.change(titleInput, { target: { value: 'Brand new' } });
    fireEvent.change(contentInput, { target: { value: 'Fresh body' } });
    // 新建默认 tags=[]，保存时不带 work
    fireEvent.click(screen.getByRole('button', { name: /保存|Save/i }));

    await waitFor(() => {
      expect(screen.getByTestId('prompt-mutation-error')).toBeTruthy();
    });
    expect(screen.queryByText('Brand new')).toBeNull();
    expect(screen.getByTestId('prompt-mutation-error').textContent).toMatch(/create-denied|创建|创建/);
    // 草稿恢复
    expect((screen.getByLabelText(/Prompt 标题|Prompt title/i) as HTMLInputElement).value).toBe(
      'Brand new',
    );
    expect((screen.getByLabelText(/Prompt 内容|Prompt content/i) as HTMLTextAreaElement).value).toBe(
      'Fresh body',
    );

    fireEvent.click(
      within(screen.getByTestId('prompt-mutation-error')).getByRole('button', {
        name: /重试|Retry/i,
      }),
    );

    await waitFor(() => {
      expect(screen.getByText('Brand new')).toBeTruthy();
    });
    expect(promptsApi.create).toHaveBeenCalledTimes(2);
    expect(promptsApi.create).toHaveBeenLastCalledWith({
      title: 'Brand new',
      content: 'Fresh body',
      tags: [],
    });
    await waitFor(() => {
      expect(screen.queryByTestId('prompt-mutation-error')).toBeNull();
    });
  });

  test('update rejection rolls back content and reopens original draft', async () => {
    vi.mocked(promptsApi.update).mockRejectedValueOnce(new Error('update-denied'));

    renderPrompts();
    await screen.findByText('Alpha title');

    const alphaCard = screen.getByTestId('prompt-card-p-1');
    fireEvent.click(within(alphaCard).getByRole('button', { name: /编辑|Edit/i }));
    const titleInput = await screen.findByLabelText(/Prompt 标题|Prompt title/i);
    fireEvent.change(titleInput, { target: { value: 'Alpha edited' } });
    fireEvent.click(screen.getByRole('button', { name: /保存|Save/i }));

    await waitFor(() => {
      expect(screen.getByTestId('prompt-mutation-error')).toBeTruthy();
    });
    // 草稿重新打开，输入框保留失败前的编辑内容
    expect((screen.getByLabelText(/Prompt 标题|Prompt title/i) as HTMLInputElement).value).toBe(
      'Alpha edited',
    );
    expect(promptsApi.update).toHaveBeenCalledWith('p-1', {
      title: 'Alpha edited',
      content: 'Alpha content body',
      tags: ['work'],
    });
    // 取消编辑后，列表应回滚为 before（Alpha title），不得保留乐观 Alpha edited
    fireEvent.click(screen.getByRole('button', { name: /取消|Cancel/i }));
    await waitFor(() => {
      expect(screen.getByText('Alpha title')).toBeTruthy();
    });
    expect(screen.queryByText('Alpha edited')).toBeNull();
  });

  test('pending entity disables conflicting edit/delete while other entities remain actionable', async () => {
    let resolveUpdate: ((value: Prompt) => void) | undefined;
    const pending = new Promise<Prompt>((resolve) => {
      resolveUpdate = resolve;
    });
    vi.mocked(promptsApi.update).mockReturnValueOnce(pending);

    renderPrompts();
    await screen.findByText('Alpha title');

    const alphaCard = screen.getByTestId('prompt-card-p-1');
    fireEvent.click(within(alphaCard).getByRole('button', { name: /编辑|Edit/i }));
    const titleInput = await screen.findByLabelText(/Prompt 标题|Prompt title/i);
    fireEvent.change(titleInput, { target: { value: 'Alpha pending' } });
    fireEvent.click(screen.getByRole('button', { name: /保存|Save/i }));

    // 乐观标题出现且同一实体动作禁用
    await waitFor(() => {
      expect(screen.getByText('Alpha pending')).toBeTruthy();
      const card = screen.getByTestId('prompt-card-p-1');
      const editBtn = within(card).getByRole('button', { name: /编辑|Edit/i }) as HTMLButtonElement;
      const deleteBtn = within(card).getByRole('button', {
        name: /删除|Delete/i,
      }) as HTMLButtonElement;
      expect(editBtn.disabled).toBe(true);
      expect(deleteBtn.disabled).toBe(true);
    });
    const betaCard = screen.getByTestId('prompt-card-p-2');
    expect(
      (within(betaCard).getByRole('button', { name: /编辑|Edit/i }) as HTMLButtonElement).disabled,
    ).toBe(false);
    expect(
      (within(betaCard).getByRole('button', { name: /删除|Delete/i }) as HTMLButtonElement)
        .disabled,
    ).toBe(false);

    resolveUpdate?.(
      buildPrompt({
        id: 'p-1',
        title: 'Alpha pending',
        content: 'Alpha content body',
        tags: ['work'],
      }),
    );
    await waitFor(() => {
      const card = screen.getByTestId('prompt-card-p-1');
      expect(
        (within(card).getByRole('button', { name: /编辑|Edit/i }) as HTMLButtonElement).disabled,
      ).toBe(false);
    });
  });

  test('delete rejection restores original index and shows error', async () => {
    vi.mocked(promptsApi.remove).mockRejectedValueOnce(new Error('delete-denied'));

    renderPrompts();
    await screen.findByText('Alpha title');

    const alphaCard = screen.getByTestId('prompt-card-p-1');
    fireEvent.click(within(alphaCard).getByRole('button', { name: /删除|Delete/i }));
    const dialog = await screen.findByRole('dialog');
    fireEvent.click(within(dialog).getByRole('button', { name: /删除|Delete/i }));

    await waitFor(() => {
      expect(screen.getByTestId('prompt-mutation-error')).toBeTruthy();
    });
    expect(screen.getByText('Alpha title')).toBeTruthy();
    expect(screen.getByText('Beta title')).toBeTruthy();
    const titles = screen.getAllByRole('heading', { level: 3 }).map((node) => node.textContent);
    const alphaIndex = titles.indexOf('Alpha title');
    const betaIndex = titles.indexOf('Beta title');
    expect(alphaIndex).toBeGreaterThanOrEqual(0);
    expect(betaIndex).toBeGreaterThan(alphaIndex);
  });

  test('retry after delete failure reuses stored payload', async () => {
    vi.mocked(promptsApi.remove)
      .mockRejectedValueOnce(new Error('delete-denied'))
      .mockResolvedValueOnce(undefined);

    renderPrompts();
    await screen.findByText('Alpha title');

    const alphaCard = screen.getByTestId('prompt-card-p-1');
    fireEvent.click(within(alphaCard).getByRole('button', { name: /删除|Delete/i }));
    const dialog = await screen.findByRole('dialog');
    fireEvent.click(within(dialog).getByRole('button', { name: /删除|Delete/i }));

    await waitFor(() => {
      expect(screen.getByTestId('prompt-mutation-error')).toBeTruthy();
    });
    expect(screen.getByText('Alpha title')).toBeTruthy();

    fireEvent.click(
      within(screen.getByTestId('prompt-mutation-error')).getByRole('button', {
        name: /重试|Retry/i,
      }),
    );

    await waitFor(() => {
      expect(screen.queryByText('Alpha title')).toBeNull();
    });
    expect(promptsApi.remove).toHaveBeenCalledTimes(2);
    expect(promptsApi.remove).toHaveBeenLastCalledWith('p-1');
    expect(screen.getByText('Beta title')).toBeTruthy();
  });

  test('sync failure is not silent and keeps existing list', async () => {
    vi.mocked(promptsApi.sync).mockRejectedValueOnce(new Error('sync-offline'));

    renderPrompts();
    await screen.findByText('Alpha title');

    fireEvent.click(screen.getByRole('button', { name: /同步|Sync/i }));

    await waitFor(() => {
      expect(screen.getByTestId('prompt-sync-error')).toBeTruthy();
    });
    expect(screen.getByText('Alpha title')).toBeTruthy();
    expect(screen.getByText('Beta title')).toBeTruthy();
    expect(screen.getByTestId('prompt-sync-error').textContent).toMatch(/sync-offline|同步/);
  });

  test('conflict pill is non-blocking and version history can restore/copy', async () => {
    const conflictVersion = buildVersion({
      id: 'v-conflict',
      kind: 'conflict',
      contentPreview: 'conflict preview body',
      content: 'conflict full body',
      sourceDevice: 'device-remote',
    });
    const historyVersion = buildVersion({
      id: 'v-history',
      kind: 'history',
      contentPreview: 'history preview body',
      content: 'history full body',
    });
    vi.mocked(promptsApi.listVersions).mockImplementation(async (id: string) => {
      if (id === 'p-1') return [conflictVersion, historyVersion];
      return [];
    });
    vi.mocked(promptsApi.restoreVersion).mockResolvedValueOnce(
      buildPrompt({
        id: 'p-1',
        title: 'Alpha restored',
        content: 'conflict full body',
        tags: ['work'],
      }),
    );

    renderPrompts();
    await screen.findByText('Alpha title');

    await waitFor(() => {
      expect(screen.getByTestId('prompt-conflict-pill-p-1')).toBeTruthy();
    });

    // 非阻塞：冲突 Pill 存在时仍可编辑
    const alphaCardBeforeEdit = screen.getByTestId('prompt-card-p-1');
    const editBtn = within(alphaCardBeforeEdit).getByRole('button', {
      name: /编辑|Edit/i,
    }) as HTMLButtonElement;
    expect(editBtn.disabled).toBe(false);
    fireEvent.click(editBtn);
    expect(await screen.findByLabelText(/Prompt 标题|Prompt title/i)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: /取消|Cancel/i }));

    // 编辑卡片卸载后需重新查询，避免对 stale 节点 click 无响应
    const alphaCard = await screen.findByTestId('prompt-card-p-1');
    fireEvent.click(within(alphaCard).getByRole('button', { name: /历史|History/i }));

    const historyPanel = await screen.findByTestId('prompts-version-history');
    expect(within(historyPanel).getByText(/conflict preview body/)).toBeTruthy();
    expect(within(historyPanel).getByText(/冲突|Conflict/)).toBeTruthy();

    fireEvent.click(
      within(screen.getByTestId('prompts-version-item-v-conflict')).getByRole('button', {
        name: /复制内容|Copy content/i,
      }),
    );
    await waitFor(() => {
      expect(navigator.clipboard.writeText).toHaveBeenCalledWith('conflict full body');
    });

    fireEvent.click(
      within(screen.getByTestId('prompts-version-item-v-conflict')).getByRole('button', {
        name: /恢复为新版本|Restore as new version/i,
      }),
    );
    fireEvent.click(screen.getByTestId('prompts-version-restore-confirm'));

    await waitFor(() => {
      expect(promptsApi.restoreVersion).toHaveBeenCalledWith('p-1', 'v-conflict');
      expect(screen.getByText('Alpha restored')).toBeTruthy();
    });
  });
});
