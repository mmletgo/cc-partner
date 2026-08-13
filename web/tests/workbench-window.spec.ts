/**
 * E2E-WORKBENCH-WINDOW-001 — 多屏卫星窗 Rail occupancy（L1 browser mock）。
 *
 * Business Logic（为什么需要这个套件）:
 *   项目可「在新窗口打开」；已被他窗占用的项目不得把本窗 active 抢走。
 *   L1 不宣称真实多屏 WebView、关主窗进程语义或双屏手感。
 *
 * Code Logic（这个套件做什么）:
 *   backendHarness mock open/claim/occupancy；断言新窗口按钮发出
 *   `open_workbench_window`，占用项目主按钮只 focus 不 touch。
 */

import { expect, test } from './fixtures';
import {
  installAppLocalStorage,
  registerAppShellCommands,
} from './support/appBootstrap';
import type { PlaywrightBackendHarness } from './support/backendHarness';

const TS = '2026-07-14T00:00:00.000Z';

/**
 * Business Logic（为什么需要这个函数）:
 *   Rail / Workbench 需要合法 project DTO。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖的 local project。
 */
function makeProject(partial: { id: string; name: string; path?: string }) {
  return {
    id: partial.id,
    name: partial.name,
    kind: 'local' as const,
    deviceId: 'device-local',
    deviceName: 'MacBook',
    path: partial.path ?? `/tmp/${partial.id}`,
    lastOpenedAt: TS,
    createdAt: TS,
    updatedAt: TS,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   worktree 条用于确认本窗项目没有被占用点击切走。
 *
 * Code Logic（这个函数做什么）:
 *   返回主 worktree DTO。
 */
function makeWorktree(partial: {
  id: string;
  projectId: string;
  name: string;
  branch: string;
}) {
  return {
    id: partial.id,
    projectId: partial.projectId,
    name: partial.name,
    branch: partial.branch,
    baseBranch: null,
    path: `/tmp/${partial.projectId}`,
    isMain: true,
    status: {
      branch: partial.branch,
      changed: 0,
      ahead: 0,
      behind: 0,
      conflicts: 0,
      clean: true,
      canPush: false,
    },
    createdAt: TS,
    updatedAt: TS,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 挂载会并发拉 worktrees/sessions/files/git。
 *
 * Code Logic（这个函数做什么）:
 *   在 AppShell 基线上注册空/就绪默认。
 */
function registerWindowBaseline(harness: PlaywrightBackendHarness): void {
  registerAppShellCommands(harness);
  harness.command('list_workbench_worktrees', { kind: 'resolve', value: [] });
  harness.command('list_workbench_dir', { kind: 'resolve', value: [] });
  harness.command('list_workbench_git_commits', { kind: 'resolve', value: [] });
  harness.command('get_focused_workbench_session', {
    kind: 'resolve',
    value: { sessionId: null },
  });
  harness.command('focus_workbench_session', {
    kind: 'resolve',
    value: { ok: true, sessionId: 'session-placeholder' },
  });
  harness.command('touch_workbench_project', {
    kind: 'resolve',
    value: makeProject({ id: 'touch', name: 'touch' }),
  });
}

test.describe('E2E-WORKBENCH-WINDOW-001 satellite occupancy', () => {
  test('new-window button opens the card project; occupied card focuses other window', async ({
    page,
    backendHarness,
  }) => {
    const projectA = makeProject({ id: 'pA', name: 'project-a' });
    const projectB = makeProject({ id: 'pB', name: 'project-b' });
    const wtA = makeWorktree({
      id: 'pA:main',
      projectId: 'pA',
      name: 'main-a',
      branch: 'main-a',
    });

    await installAppLocalStorage(page);
    registerWindowBaseline(backendHarness);
    backendHarness.command('list_workbench_projects', {
      kind: 'resolve',
      value: [projectA, projectB],
    });
    backendHarness.command('list_workbench_worktrees', {
      kind: 'resolve',
      value: [wtA],
    });
    backendHarness.command('list_workbench_window_occupancy', {
      kind: 'resolve',
      value: [
        { projectId: 'pA', windowLabel: 'main' },
        { projectId: 'pB', windowLabel: 'workbench-1' },
      ],
    });
    backendHarness.command('claim_workbench_window_project', {
      kind: 'resolve',
      value: { action: 'claimed', label: 'main', projectId: 'pA' },
    });
    backendHarness.command('open_workbench_window', {
      kind: 'resolve',
      value: { action: 'created', label: 'workbench-2', projectId: 'pB' },
    });
    backendHarness.command('touch_workbench_project', {
      kind: 'resolve',
      value: projectA,
    });

    await page.addInitScript(() => {
      window.localStorage.setItem('cp-workbench-active-project-id', 'pA');
    });

    await page.goto('/workbench?projectId=pA');
    await expect(page.getByRole('region', { name: 'Worktree 管理' })).toBeVisible({
      timeout: 20_000,
    });
    await expect(page.getByText('已在其他窗口')).toBeVisible({ timeout: 10_000 });

    await page
      .locator('[data-project-id="pB"]')
      .getByTestId('project-open-new-window')
      .click();

    await expect
      .poll(
        () =>
          backendHarness
            .calls()
            .filter(
              (call) =>
                call.type === 'invoke' && call.command === 'open_workbench_window',
            ),
        { timeout: 5_000 },
      )
      .toEqual(
        expect.arrayContaining([
          expect.objectContaining({
            type: 'invoke',
            command: 'open_workbench_window',
            args: { projectId: 'pB' },
          }),
        ]),
      );

    backendHarness.command('claim_workbench_window_project', {
      kind: 'resolve',
      value: { action: 'occupied', label: 'workbench-1', projectId: 'pB' },
    });

    const touchBefore = backendHarness
      .calls()
      .filter((call) => call.type === 'invoke' && call.command === 'touch_workbench_project')
      .length;

    await page.getByRole('button', { name: /project-b/ }).click();

    await expect
      .poll(
        () =>
          backendHarness
            .calls()
            .filter(
              (call) =>
                call.type === 'invoke' &&
                call.command === 'claim_workbench_window_project' &&
                Boolean(
                  call.args &&
                    typeof call.args === 'object' &&
                    (call.args as { projectId?: string }).projectId === 'pB',
                ),
            ).length,
        { timeout: 5_000 },
      )
      .toBeGreaterThan(0);

    await expect
      .poll(
        () =>
          backendHarness
            .calls()
            .filter(
              (call) =>
                call.type === 'invoke' &&
                call.command === 'focus_workbench_window' &&
                Boolean(
                  call.args &&
                    typeof call.args === 'object' &&
                    (call.args as { label?: string }).label === 'workbench-1',
                ),
            ).length,
        { timeout: 5_000 },
      )
      .toBeGreaterThan(0);

    expect(
      backendHarness
        .calls()
        .filter((call) => call.type === 'invoke' && call.command === 'touch_workbench_project')
        .length,
    ).toBe(touchBefore);
    await expect(page.getByRole('region', { name: 'Worktree 管理' })).toContainText(
      'main-a',
    );
  });
});
