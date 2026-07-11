import { describe, test } from 'vitest';
import type { WorkbenchSession } from '@/lib/types';
import { projectSessionStats, sessionStatsByProject } from '@/lib/workbenchProjectStats';

/**
 * Business Logic（为什么需要这个函数）:
 *   项目卡片统计测试只关心项目、window 数和 pane 数，不需要构造真实终端。
 *
 * Code Logic（这个函数做什么）:
 *   构造满足统计测试所需字段的 WorkbenchSession；paneCount 表示后端返回的真实 tmux pane 数。
 */
function session(id: string, projectId: string, paneCount: number): WorkbenchSession {
  return {
    id,
    projectId,
    worktreeId: null,
    name: id,
    command: '/bin/zsh',
    cwd: '/repo',
    status: 'running',
    cols: 120,
    rows: 30,
    startedAt: '2026-06-25T00:00:00.000Z',
    exitedAt: null,
    exitCode: null,
    supportsPanes: true,
    paneCount,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试失败时需要直接看到实际统计值，方便定位项目卡片显示错误。
 *
 * Code Logic（这个函数做什么）:
 *   使用 JSON 严格比较 actual 与 expected，不一致时抛出包含两边值的错误。
 */
function assertJson(actual: unknown, expected: unknown): void {
  const actualJson = JSON.stringify(actual);
  const expectedJson = JSON.stringify(expected);
  if (actualJson !== expectedJson) {
    throw new Error(`Expected ${expectedJson}, got ${actualJson}`);
  }
}

describe('projectSessionStats', () => {
  test('aggregates window and pane counts by project, handling missing and NaN panes', () => {
    const sessions = [
      session('window-1', 'project-1', 1),
      session('window-2', 'project-1', 3),
      session('window-3', 'project-2', 2),
    ];

    assertJson(sessionStatsByProject(sessions), {
      'project-1': { windowCount: 2, paneCount: 4 },
      'project-2': { windowCount: 1, paneCount: 2 },
    });

    assertJson(projectSessionStats(sessions, 'project-3'), { windowCount: 0, paneCount: 0 });

    assertJson(sessionStatsByProject([session('window-4', 'project-4', Number.NaN)]), {
      'project-4': { windowCount: 1, paneCount: 1 },
    });
  });
});
