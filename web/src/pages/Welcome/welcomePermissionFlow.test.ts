// web/src/pages/Welcome/welcomePermissionFlow.test.ts
import { describe, expect, test } from 'vitest';
import {
  hasStickyDenied,
  isStickyPermission,
  reduceWelcomePermPhase,
  welcomeHintKey,
} from './welcomePermissionFlow';

const denied = {
  screenCapture: { granted: false },
  accessibility: { granted: false },
  inputMonitoring: { granted: false },
};
const stickyOk = {
  screenCapture: { granted: true },
  accessibility: { granted: true },
  inputMonitoring: { granted: true },
};

describe('welcomePermissionFlow', () => {
  test('notification is not sticky; screenCapture is', () => {
    expect(isStickyPermission('notification')).toBe(false);
    expect(isStickyPermission('screenCapture')).toBe(true);
  });

  test('GO_SETTINGS on sticky moves idle -> awaiting', () => {
    expect(
      reduceWelcomePermPhase('idle', { type: 'GO_SETTINGS', permission: 'inputMonitoring' }),
    ).toBe('awaiting');
  });

  test('GO_SETTINGS on sticky keeps needs_reopen (button must not vanish)', () => {
    expect(
      reduceWelcomePermPhase('needs_reopen', {
        type: 'GO_SETTINGS',
        permission: 'screenCapture',
      }),
    ).toBe('needs_reopen');
  });

  test('GO_SETTINGS on notification stays idle', () => {
    expect(
      reduceWelcomePermPhase('idle', { type: 'GO_SETTINGS', permission: 'notification' }),
    ).toBe('idle');
  });

  test('FOREGROUND only from awaiting -> syncing; keeps needs_reopen', () => {
    expect(reduceWelcomePermPhase('awaiting', { type: 'FOREGROUND' })).toBe('syncing');
    expect(reduceWelcomePermPhase('idle', { type: 'FOREGROUND' })).toBe('idle');
    expect(reduceWelcomePermPhase('needs_reopen', { type: 'FOREGROUND' })).toBe('needs_reopen');
  });

  test('USER_RECHECK from idle enters syncing; keeps needs_reopen', () => {
    expect(reduceWelcomePermPhase('idle', { type: 'USER_RECHECK' })).toBe('syncing');
    expect(reduceWelcomePermPhase('awaiting', { type: 'USER_RECHECK' })).toBe('syncing');
    expect(reduceWelcomePermPhase('needs_reopen', { type: 'USER_RECHECK' })).toBe('needs_reopen');
  });

  test('BACKEND_NEEDS_RELAUNCH immediately enters needs_reopen from any phase', () => {
    expect(reduceWelcomePermPhase('idle', { type: 'BACKEND_NEEDS_RELAUNCH' })).toBe(
      'needs_reopen',
    );
    expect(reduceWelcomePermPhase('awaiting', { type: 'BACKEND_NEEDS_RELAUNCH' })).toBe(
      'needs_reopen',
    );
    expect(reduceWelcomePermPhase('syncing', { type: 'BACKEND_NEEDS_RELAUNCH' })).toBe(
      'needs_reopen',
    );
  });

  test('SYNC_TICK with sticky granted ends at idle', () => {
    expect(reduceWelcomePermPhase('syncing', { type: 'SYNC_TICK', status: stickyOk })).toBe(
      'idle',
    );
  });

  test('SYNC_TICK on needs_reopen with sticky denied keeps needs_reopen', () => {
    expect(
      reduceWelcomePermPhase('needs_reopen', { type: 'SYNC_TICK', status: denied }),
    ).toBe('needs_reopen');
  });

  test('SYNC_EXHAUSTED with sticky denied -> needs_reopen', () => {
    expect(
      reduceWelcomePermPhase('syncing', { type: 'SYNC_EXHAUSTED', status: denied }),
    ).toBe('needs_reopen');
  });

  test('REOPEN_CLICKED does not change phase (side-effect elsewhere)', () => {
    expect(reduceWelcomePermPhase('needs_reopen', { type: 'REOPEN_CLICKED' })).toBe(
      'needs_reopen',
    );
  });

  test('hint keys', () => {
    expect(welcomeHintKey('idle', true)).toBe('permissionReady');
    expect(welcomeHintKey('idle', false)).toBe('waitingPermission');
    expect(welcomeHintKey('syncing', false)).toBe('syncingPermission');
    expect(welcomeHintKey('needs_reopen', false)).toBe('reopenToApply');
  });

  test('hasStickyDenied', () => {
    expect(hasStickyDenied(denied)).toBe(true);
    expect(hasStickyDenied(stickyOk)).toBe(false);
  });
});
