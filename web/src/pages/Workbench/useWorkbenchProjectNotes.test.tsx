// @vitest-environment jsdom
/**
 * useWorkbenchProjectNotes 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   项目笔记只在 notes tab 拉取，切项目必须 flush 旧页，编辑走 debounce。
 *
 * Code Logic（这个测试做什么）:
 *   mock workbenchApi.notes；覆盖未打开不请求、打开后加载、编辑 schedule、切项目 flush。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';

import { useWorkbenchProjectNotes } from './useWorkbenchProjectNotes';

const notesApi = vi.hoisted(() => ({
  get: vi.fn(),
  save: vi.fn(),
}));

vi.mock('@/api/workbench', () => ({
  workbenchApi: {
    notes: notesApi,
  },
}));

vi.mock('@/lib/pendingWrites', () => ({
  pendingWrites: {
    register: vi.fn(() => () => undefined),
  },
}));

describe('useWorkbenchProjectNotes', () => {
  beforeEach(() => {
    notesApi.get.mockReset();
    notesApi.save.mockReset();
    notesApi.get.mockResolvedValue({ projectId: 'p1', content: '# hello', updatedAt: 't1' });
    notesApi.save.mockResolvedValue({ projectId: 'p1', content: 'saved', updatedAt: 't2' });
  });

  afterEach(() => {
    cleanup();
  });

  test('does not fetch until notes tab is active', async () => {
    renderHook(() =>
      useWorkbenchProjectNotes({
        activeProjectId: 'p1',
        inspectorTab: 'history',
        desktopUnavailableMessage: 'desktop down',
        loadFailedFallback: 'load failed',
      }),
    );
    await act(async () => {
      await Promise.resolve();
    });
    expect(notesApi.get).not.toHaveBeenCalled();
  });

  test('loads note when notes tab opens', async () => {
    const { result } = renderHook(() =>
      useWorkbenchProjectNotes({
        activeProjectId: 'p1',
        inspectorTab: 'notes',
        desktopUnavailableMessage: 'desktop down',
        loadFailedFallback: 'load failed',
      }),
    );
    await waitFor(() => {
      expect(notesApi.get).toHaveBeenCalledWith('p1');
    });
    await waitFor(() => {
      expect(result.current.content).toBe('# hello');
      expect(result.current.loading).toBe(false);
    });
  });

  test('schedules save and flushes previous project on switch', async () => {
    notesApi.get.mockImplementation(async (projectId: string) => ({
      projectId,
      content: projectId === 'p1' ? 'one' : 'two',
      updatedAt: 't',
    }));
    const { result, rerender } = renderHook(
      (props: { projectId: string }) =>
        useWorkbenchProjectNotes({
          activeProjectId: props.projectId,
          inspectorTab: 'notes',
          desktopUnavailableMessage: 'desktop down',
          loadFailedFallback: 'load failed',
        }),
      { initialProps: { projectId: 'p1' } },
    );
    await waitFor(() => {
      expect(result.current.content).toBe('one');
    });

    act(() => {
      result.current.onChange('edited');
    });
    rerender({ projectId: 'p2' });
    await waitFor(() => {
      expect(notesApi.save).toHaveBeenCalledWith('p1', 'edited');
    });
    await waitFor(() => {
      expect(result.current.content).toBe('two');
    });
    expect(notesApi.save).not.toHaveBeenCalledWith('p2', expect.anything());
  });
});
