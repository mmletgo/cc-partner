// @vitest-environment jsdom
/** Workbench 收藏快捷输入 hook：事件级打开加载与终端注入回归。 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import type { RefObject } from 'react';
import type { Prompt } from '@/lib/types';
import { useWorkbenchFavoriteQuickInput } from './useWorkbenchFavoriteQuickInput';

const apiMocks = vi.hoisted(() => ({
  getConfig: vi.fn(),
  listPrompts: vi.fn(),
}));

vi.mock('@/api/config', () => ({ configApi: { get: apiMocks.getConfig } }));
vi.mock('@/api/prompts', () => ({ promptsApi: { list: apiMocks.listPrompts } }));

const favorite: Prompt = {
  id: 'favorite',
  title: 'Favorite',
  content: 'insert me',
  tags: [],
  favorite: true,
  updatedAt: '2026-08-14T00:00:00Z',
};
const regular: Prompt = { ...favorite, id: 'regular', title: 'Regular', favorite: false };

describe('useWorkbenchFavoriteQuickInput', () => {
  beforeEach(() => {
    apiMocks.getConfig.mockReset();
    apiMocks.listPrompts.mockReset();
    apiMocks.getConfig.mockResolvedValue({ promptQuickInputHotkey: '<ctrl>+/' });
    apiMocks.listPrompts.mockResolvedValue([favorite, regular]);
  });

  afterEach(() => cleanup());

  test('loads only when opening and inserts favorite content without Enter', async () => {
    const handleInput = vi.fn();
    const terminal = document.createElement('section');
    const terminalPanelRef = { current: terminal } as RefObject<HTMLElement | null>;
    const { result } = renderHook(() =>
      useWorkbenchFavoriteQuickInput({
        activeSessionId: 'session-1',
        terminalPanelRef,
        handleInput,
      }),
    );

    expect(apiMocks.listPrompts).not.toHaveBeenCalled();
    act(() => result.current.onToggle());
    await waitFor(() => expect(result.current.favoritePrompts).toEqual([favorite]));
    expect(apiMocks.listPrompts).toHaveBeenCalledTimes(1);

    act(() => result.current.onToggle());
    expect(result.current.open).toBe(false);
    expect(apiMocks.listPrompts).toHaveBeenCalledTimes(1);

    act(() => result.current.onToggle());
    await waitFor(() => expect(apiMocks.listPrompts).toHaveBeenCalledTimes(2));
    act(() => result.current.onSelectPrompt(favorite));
    expect(handleInput).toHaveBeenCalledWith('session-1', 'insert me');
    expect(result.current.open).toBe(false);
  });
});
