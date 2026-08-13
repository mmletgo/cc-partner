/**
 * workspaceLayout pure coordinator 测试。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  buildWorkspaceLayoutDraft,
  DESKTOP_AUTO_SLOT_KEY,
  WorkspaceLayoutAutosaveCoordinator,
  type WorkspaceLayout,
  type WorkspaceLayoutDraft,
  type WorkspaceLayoutSelection,
} from './workspaceLayout';

function baseSelection(
  overrides: Partial<WorkspaceLayoutSelection> = {},
): WorkspaceLayoutSelection {
  return {
    projectId: 'p1',
    activeWorktreeId: 'w1',
    activeSessionId: 's1',
    workspaceView: 'terminal',
    inspectorTab: 'files',
    browserTargetUrl: null,
    slotKey: DESKTOP_AUTO_SLOT_KEY,
    ...overrides,
  };
}

function layoutFromDraft(draft: WorkspaceLayoutDraft, revision: number): WorkspaceLayout {
  return {
    ...draft,
    schemaVersion: 1,
    id: 'L1',
    revision,
    createdAt: 't0',
    updatedAt: 't1',
  };
}

describe('buildWorkspaceLayoutDraft', () => {
  it('returns null without project and excludes content fields', () => {
    expect(buildWorkspaceLayoutDraft(baseSelection({ projectId: null }))).toBeNull();
    const draft = buildWorkspaceLayoutDraft(baseSelection())!;
    expect(draft.slotKey).toBe(DESKTOP_AUTO_SLOT_KEY);
    expect(JSON.stringify(draft)).not.toContain('secret');
    expect(draft).not.toHaveProperty('command');
    expect(draft).not.toHaveProperty('prompt');
  });

  it('writes satellite window auto slot instead of desktop:auto', () => {
    const draft = buildWorkspaceLayoutDraft(
      baseSelection({ slotKey: 'desktop:auto:window:workbench-2' }),
    )!;
    expect(draft.slotKey).toBe('desktop:auto:window:workbench-2');
    expect(draft.kind).toBe('auto');
  });
});

describe('WorkspaceLayoutAutosaveCoordinator', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('coalesces stable selection changes and excludes content events', async () => {
    vi.useFakeTimers();
    const saved: WorkspaceLayoutDraft[] = [];
    let selection = baseSelection({ activeWorktreeId: null });
    const fixture = new WorkspaceLayoutAutosaveCoordinator({
      save: async (draft, expected) => {
        const next = layoutFromDraft(draft, (expected ?? 0) + 1);
        saved.push(draft);
        return next;
      },
      get: async () => null,
      select: () => selection,
      schedule: (fn, ms) => setTimeout(fn, ms) as unknown as ReturnType<typeof setTimeout>,
      clearSchedule: (id) => clearTimeout(id as unknown as NodeJS.Timeout),
    });

    selection = baseSelection({ projectId: 'p1', activeWorktreeId: null });
    fixture.notifySelectionChanged();
    selection = baseSelection({ projectId: 'p1', activeWorktreeId: 'w1' });
    fixture.notifySelectionChanged();
    fixture.notifyContentNoise();
    // terminal output 等噪声
    expect(saved).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(499);
    expect(saved).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(1);
    expect(saved).toEqual([
      expect.objectContaining({ projectId: 'p1', activeWorktreeId: 'w1' }),
    ]);
    expect(JSON.stringify(saved)).not.toContain('secret');
    fixture.dispose();
  });

  it('does not write empty layout when project is missing', async () => {
    vi.useFakeTimers();
    const save = vi.fn();
    const fixture = new WorkspaceLayoutAutosaveCoordinator({
      save,
      get: async () => null,
      select: () => baseSelection({ projectId: null }),
    });
    fixture.notifySelectionChanged();
    await vi.advanceTimersByTimeAsync(500);
    expect(save).not.toHaveBeenCalled();
    fixture.dispose();
  });

  it('rereads and recomputes on CAS conflict', async () => {
    let selection = baseSelection({ projectId: 'p1' });
    let revision: number | null = 1;
    const server: { layout: WorkspaceLayout | null } = {
      layout: layoutFromDraft(buildWorkspaceLayoutDraft(selection)!, 1),
    };
    const saved: WorkspaceLayoutDraft[] = [];
    const fixture = new WorkspaceLayoutAutosaveCoordinator({
      save: async (draft, expected) => {
        if (expected !== revision) {
          throw { code: 'workspace_layout_conflict' };
        }
        revision = (expected ?? 0) + 1;
        const next = layoutFromDraft(draft, revision);
        server.layout = next;
        saved.push(draft);
        return next;
      },
      get: async () => server.layout,
      select: () => selection,
      debounceMs: 0,
    });
    await fixture.hydrateRevision();
    // 并发写把 revision 推到 2
    revision = 2;
    server.layout = layoutFromDraft(buildWorkspaceLayoutDraft(selection)!, 2);
    selection = baseSelection({ projectId: 'p2' });
    await fixture.flush();
    expect(saved.at(-1)?.projectId).toBe('p2');
    expect(revision).toBe(3);
  });

  it('pause cancels a pending debounce and blocks flush until resume', async () => {
    vi.useFakeTimers();
    const save = vi.fn(async (draft: WorkspaceLayoutDraft, expected: number | null) =>
      layoutFromDraft(draft, (expected ?? 0) + 1),
    );
    const fixture = new WorkspaceLayoutAutosaveCoordinator({
      save,
      get: async () => null,
      select: () => baseSelection(),
    });
    fixture.notifySelectionChanged();
    fixture.pause();
    await vi.advanceTimersByTimeAsync(600);
    expect(save).not.toHaveBeenCalled();
    await fixture.flush();
    expect(save).not.toHaveBeenCalled();
    fixture.resume();
    fixture.notifySelectionChanged();
    await vi.advanceTimersByTimeAsync(500);
    expect(save).toHaveBeenCalledTimes(1);
    fixture.dispose();
  });
});
