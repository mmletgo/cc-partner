import { describe, expect, test, vi } from 'vitest';
import {
  openWorkbenchDeepLink,
  parseWorkbenchUrlAsDeepLink,
  resolveWorkbenchNavigationTarget,
} from './workbenchWindowNavigation';

const occupancy = [
  { projectId: 'p-main', windowLabel: 'main' },
  { projectId: 'p-sat', windowLabel: 'workbench-1' },
];

describe('resolveWorkbenchNavigationTarget', () => {
  test('classifies local / other / unoccupied', () => {
    expect(resolveWorkbenchNavigationTarget('p-main', 'main', occupancy)).toEqual({
      kind: 'local',
    });
    expect(resolveWorkbenchNavigationTarget('p-sat', 'main', occupancy)).toEqual({
      kind: 'other',
      label: 'workbench-1',
    });
    expect(resolveWorkbenchNavigationTarget('p-new', 'main', occupancy)).toEqual({
      kind: 'unoccupied',
    });
    expect(resolveWorkbenchNavigationTarget(null, 'main', occupancy)).toEqual({ kind: 'local' });
  });
});

describe('openWorkbenchDeepLink', () => {
  test('navigates locally when this window owns the project', async () => {
    const navigate = vi.fn();
    const result = await openWorkbenchDeepLink({
      target: { projectId: 'p-main', worktreeId: null, sessionId: null },
      currentLabel: 'main',
      occupancy,
      navigate,
      claim: vi.fn(),
      focus: vi.fn(),
      applyOnWindow: vi.fn(),
    });
    expect(result).toBe('local');
    expect(navigate).toHaveBeenCalledWith('/workbench?projectId=p-main');
  });

  test('focuses the occupying window instead of switching this one', async () => {
    const navigate = vi.fn();
    const focus = vi.fn(async () => undefined);
    const applyOnWindow = vi.fn(async () => undefined);
    const result = await openWorkbenchDeepLink({
      target: { projectId: 'p-sat', worktreeId: 'w1', sessionId: 's1' },
      currentLabel: 'main',
      occupancy,
      navigate,
      claim: vi.fn(),
      focus,
      applyOnWindow,
    });
    expect(result).toBe('focused-other');
    expect(navigate).not.toHaveBeenCalled();
    expect(focus).toHaveBeenCalledWith('workbench-1');
    expect(applyOnWindow).toHaveBeenCalledWith(
      'workbench-1',
      expect.objectContaining({ projectId: 'p-sat' }),
    );
  });

  test('claims unoccupied project then navigates locally', async () => {
    const navigate = vi.fn();
    const claim = vi.fn(async () => ({ action: 'claimed', label: 'main' }));
    const result = await openWorkbenchDeepLink({
      target: { projectId: 'p-new', worktreeId: null, sessionId: null },
      currentLabel: 'main',
      occupancy,
      navigate,
      claim,
      focus: vi.fn(),
      applyOnWindow: vi.fn(),
    });
    expect(result).toBe('claimed');
    expect(claim).toHaveBeenCalledWith('p-new');
    expect(navigate).toHaveBeenCalledWith('/workbench?projectId=p-new');
  });
});

describe('parseWorkbenchUrlAsDeepLink', () => {
  test('only parses workbench urls', () => {
    expect(parseWorkbenchUrlAsDeepLink('/settings?tab=dependencies')).toBeNull();
    expect(parseWorkbenchUrlAsDeepLink('/workbench?projectId=p1')?.projectId).toBe('p1');
  });
});
