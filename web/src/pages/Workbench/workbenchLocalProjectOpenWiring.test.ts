import { describe, test } from 'vitest';
import { readFileSync } from 'node:fs';

/**
 * Business Logic（为什么需要这个函数）:
 *   源码契约失败时需要可定位原因，而不是 vitest 的 opaque assertion。
 *
 * Code Logic（这个函数做什么）:
 *   条件为 false 时抛出带说明的 Error。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

describe('workbench local project open wiring', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   本机选择器若自己调 projects.add，共享列表不会更新，用户只能点刷新。
   *
   * Code Logic（这个测试做什么）:
   *   断言 context 暴露 addProjectFromPath，侧栏与启动面把它注入 local picker。
   */
  test('desktop local pickers inject addProjectFromPath so the shared list updates without refresh', () => {
    const context = readFileSync(
      new URL('../../hooks/workbenchProjectsContext.ts', import.meta.url),
      'utf8',
    );
    const provider = readFileSync(
      new URL('../../hooks/useWorkbenchProjects.tsx', import.meta.url),
      'utf8',
    );
    const rail = readFileSync(
      new URL('../../components/domain/WorkbenchProjectRail/WorkbenchProjectRail.tsx', import.meta.url),
      'utf8',
    );
    const launch = readFileSync(new URL('./WorkbenchLaunchSurface.tsx', import.meta.url), 'utf8');

    assert(
      context.includes('addProjectFromPath: (path: string) => Promise<WorkbenchProject | null>'),
      'shared project context must expose addProjectFromPath',
    );
    assert(
      provider.includes('addProjectFromPath,'),
      'provider must put addProjectFromPath on the shared context value',
    );
    assert(
      rail.includes('openLocalProject={addProjectFromPath}'),
      'project rail local picker must inject addProjectFromPath',
    );
    const launchInjects = launch.split('openLocalProject={addProjectFromPath}').length - 1;
    assert(
      launchInjects === 2,
      'launch surface empty and continue local pickers must both inject addProjectFromPath',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   添加项目后若只进 `/workbench`，restore 会用上次 layout 盖掉刚打开的项目。
   *
   * Code Logic（这个测试做什么）:
   *   断言侧栏与启动面的 onProjectOpened 把 project.id 写进 workbench deep link。
   */
  test('opening a project from pickers navigates with the new projectId', () => {
    const rail = readFileSync(
      new URL('../../components/domain/WorkbenchProjectRail/WorkbenchProjectRail.tsx', import.meta.url),
      'utf8',
    );
    const launch = readFileSync(new URL('./WorkbenchLaunchSurface.tsx', import.meta.url), 'utf8');

    assert(
      !rail.includes("navigate('/workbench')"),
      'project rail must not navigate to bare /workbench after opening a project',
    );
    assert(
      rail.includes('onProjectOpened={(project) =>') && rail.includes('projectId: project.id'),
      'project rail picker must put the opened project id on the workbench URL',
    );
    assert(
      launch.includes('onProjectOpened={(project) =>') && launch.includes('projectId: project.id'),
      'launch surface picker must put the opened project id on the workbench URL',
    );
  });
});
