/**
 * Workspace Safe Restore E2E scenarios (fixture pending full app restart harness).
 *
 * Scenarios covered when harness lands:
 * - local restart with existing tmux target
 * - stale session partial notice once
 * - remote owner offline partial
 * - old peer unsupported capability
 * - full success silent
 * - dirty editor unchanged
 * - named snapshot CRUD
 * - restore path counters: terminal write / tmux create / worktree create / agent spawn = 0
 */
import { test, expect } from '@playwright/test';

test.describe('workspace safe restore', () => {
  test.skip(true, 'App restart fixture not yet wired in this worktree; tracked as E2E-WORKSPACE-RESTORE-001 NOT VERIFIED');

  test('placeholder keeps suite discoverable', async () => {
    expect(true).toBe(true);
  });
});
