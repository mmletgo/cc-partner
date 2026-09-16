import { describe, expect, test } from 'vitest';
import {
  MAX_MOUNTED_MOBILE_TERMINALS,
  nextMountedMobileSessionIds,
} from './mobileMountedTerminals';

describe('nextMountedMobileSessionIds', () => {
  test('always keeps the active session even when over cap', () => {
    const preferred = Array.from({ length: MAX_MOUNTED_MOBILE_TERMINALS + 3 }, (_, index) => `s${index}`);
    const mounted = nextMountedMobileSessionIds({
      previous: [],
      preferred,
      activeId: 's10',
      liveIds: preferred,
    });
    expect(mounted).toContain('s10');
    expect(mounted).toHaveLength(MAX_MOUNTED_MOBILE_TERMINALS);
  });

  test('prefers current-project sessions then previously mounted ids', () => {
    expect(
      nextMountedMobileSessionIds({
        previous: ['old-a', 'old-b'],
        preferred: ['cur-1', 'cur-2'],
        activeId: 'cur-2',
        liveIds: ['cur-1', 'cur-2', 'old-a', 'old-b'],
      }),
    ).toEqual(['cur-2', 'cur-1', 'old-a', 'old-b']);
  });

  test('drops ids that left preferred and previous', () => {
    expect(
      nextMountedMobileSessionIds({
        previous: ['gone', 'keep'],
        preferred: ['keep', 'new'],
        activeId: 'new',
        liveIds: ['keep', 'new'],
      }),
    ).toEqual(['new', 'keep']);
  });

  test('evicts oldest inactive sessions when over cap', () => {
    const previous = Array.from({ length: MAX_MOUNTED_MOBILE_TERMINALS }, (_, index) => `old${index}`);
    const mounted = nextMountedMobileSessionIds({
      previous,
      preferred: ['fresh'],
      activeId: 'fresh',
      liveIds: ['fresh', ...previous],
    });
    expect(mounted[0]).toBe('fresh');
    expect(mounted).toHaveLength(MAX_MOUNTED_MOBILE_TERMINALS);
    expect(mounted).not.toContain(`old${MAX_MOUNTED_MOBILE_TERMINALS - 1}`);
  });
});
