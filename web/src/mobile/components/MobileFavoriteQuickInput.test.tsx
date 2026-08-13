// @vitest-environment jsdom
/**
 * MobileFavoriteQuickInput 回归测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   每次打开必须刷新收藏，同时关闭重开不能丢失用户的搜索条件。
 *
 * Code Logic（这个测试做什么）:
 *   mock HTTP Prompt 列表，验证首开加载、关闭重开再次加载与外层筛选状态保留。
 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import type { Prompt } from '@/lib/types';
import { MobileFavoriteQuickInput } from './MobileFavoriteQuickInput';

const apiMocks = vi.hoisted(() => ({ list: vi.fn() }));

vi.mock('@/api/workbenchHttp', () => ({
  httpWorkbenchTransport: { prompts: { list: apiMocks.list } },
}));

const favorite: Prompt = {
  id: 'prompt-1',
  title: 'Release checklist',
  content: 'Run the release checks',
  tags: ['release'],
  favorite: true,
  updatedAt: '2026-08-14T00:00:00Z',
};

describe('MobileFavoriteQuickInput', () => {
  beforeEach(async () => {
    apiMocks.list.mockReset();
    apiMocks.list.mockResolvedValue([favorite]);
    await i18n.changeLanguage('en');
  });

  afterEach(() => cleanup());

  test('reloads on every open while preserving the search query', async () => {
    const view = render(
      <I18nextProvider i18n={i18n}>
        <MobileFavoriteQuickInput open onClose={vi.fn()} onSelectPrompt={vi.fn()} />
      </I18nextProvider>,
    );

    await screen.findByRole('button', { name: 'Insert prompt: Release checklist' });
    const search = screen.getByRole('searchbox', { name: 'Search favorite prompts' });
    fireEvent.change(search, { target: { value: 'release' } });
    expect(apiMocks.list).toHaveBeenCalledTimes(1);

    view.rerender(
      <I18nextProvider i18n={i18n}>
        <MobileFavoriteQuickInput open={false} onClose={vi.fn()} onSelectPrompt={vi.fn()} />
      </I18nextProvider>,
    );
    view.rerender(
      <I18nextProvider i18n={i18n}>
        <MobileFavoriteQuickInput open onClose={vi.fn()} onSelectPrompt={vi.fn()} />
      </I18nextProvider>,
    );

    await waitFor(() => expect(apiMocks.list).toHaveBeenCalledTimes(2));
    expect(
      (screen.getByRole('searchbox', {
        name: 'Search favorite prompts',
      }) as HTMLInputElement).value,
    ).toBe('release');
  });
});
