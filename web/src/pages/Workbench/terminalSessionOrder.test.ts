import type { WorkbenchSession } from '../../lib/types';
import { visibleTerminalSessions } from './terminalSessionOrder';

/**
 * Business Logic（为什么需要这个函数）:
 *   工作台终端排序测试需要快速构造不同创建时间的会话，验证多终端布局不会被点击焦点重排。
 *
 * Code Logic（这个函数做什么）:
 *   接收 id、name 和 startedAt，返回满足排序测试所需字段的 WorkbenchSession。
 */
function session(id: string, name: string, startedAt: string): WorkbenchSession {
  return {
    id,
    projectId: 'project-1',
    name,
    command: 'claude',
    status: 'running',
    cols: 120,
    rows: 30,
    startedAt,
    exitedAt: null,
    exitCode: null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   排序测试只关心会话 id 顺序，需要用清晰错误展示实际可见终端列表。
 *
 * Code Logic（这个函数做什么）:
 *   从 WorkbenchSession 数组投影 id，并与期望 id 列表做 JSON 严格比较。
 */
function assertIds(actual: WorkbenchSession[], expected: string[]): void {
  const actualIds = actual.map((item) => item.id);
  const actualJson = JSON.stringify(actualIds);
  const expectedJson = JSON.stringify(expected);
  if (actualJson !== expectedJson) {
    throw new Error(`Expected ${expectedJson}, got ${actualJson}`);
  }
}

const sessions = [
  session('second', 'Terminal 2', '2026-06-24T09:05:00.000Z'),
  session('third', 'Terminal 3', '2026-06-24T09:10:00.000Z'),
  session('first', 'Terminal 1', '2026-06-24T09:00:00.000Z'),
  session('fourth', 'Terminal 4', '2026-06-24T09:15:00.000Z'),
  session('fifth', 'Terminal 5', '2026-06-24T09:20:00.000Z'),
];

assertIds(
  visibleTerminalSessions({
    sessions,
    activeSessionId: 'third',
    layout: 'double',
  }),
  ['first', 'second'],
);

assertIds(
  visibleTerminalSessions({
    sessions,
    activeSessionId: 'fifth',
    layout: 'quad',
  }),
  ['first', 'second', 'third', 'fourth'],
);

assertIds(
  visibleTerminalSessions({
    sessions,
    activeSessionId: 'third',
    layout: 'single',
  }),
  ['third'],
);

assertIds(
  visibleTerminalSessions({
    sessions,
    activeSessionId: null,
    layout: 'single',
  }),
  ['first'],
);

console.log('terminalSessionOrder.test.ts passed');
