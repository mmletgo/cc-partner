import type { WorkbenchProject, WorkbenchSession } from '../../lib/types';
import {
  canFillPromptIntoTerminal,
  resetPromptOptimizerTextState,
  createPromptOptimizerShortcutState,
  promptOptimizerInsertPayload,
  promptOptimizerInputKeyAction,
  shouldCommitPromptOptimizerPanelPosition,
  promptOptimizerShortcutAction,
  reducePromptOptimizerShortcut,
  promptOptimizerWorkingDirectory,
  selectPromptOptimizerInsertText,
} from './promptOptimizerWidget';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench prompt 优化小组件测试需要构造不同运行状态的终端会话。
 *
 * Code Logic（这个函数做什么）:
 *   接收 status，返回满足 helper 测试所需字段的 WorkbenchSession。
 */
function session(status: WorkbenchSession['status']): WorkbenchSession {
  return {
    id: 'session-1',
    projectId: 'project-1',
    worktreeId: null,
    name: 'Terminal 1',
    command: 'claude',
    cwd: '/repo',
    status,
    cols: 120,
    rows: 30,
    startedAt: '2026-06-24T09:00:00.000Z',
    exitedAt: null,
    exitCode: null,
    supportsPanes: true,
    paneCount: 1,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench prompt 优化小组件需要当前项目根目录，以便 Claude Code 加载项目 CLAUDE.md。
 *
 * Code Logic（这个函数做什么）:
 *   返回满足 helper 测试所需字段的 WorkbenchProject。
 */
function project(path: string): WorkbenchProject {
  return {
    id: 'project-1',
    name: 'Pando',
    kind: 'local',
    deviceId: 'device-1',
    deviceName: 'Mac',
    path,
    lastOpenedAt: '2026-06-24T09:00:00.000Z',
    createdAt: '2026-06-24T09:00:00.000Z',
    updatedAt: '2026-06-24T09:00:00.000Z',
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   纯函数测试不引入测试框架，需要在失败时给出清晰上下文。
 *
 * Code Logic（这个函数做什么）:
 *   对比 JSON 序列化后的实际值和期望值，不一致则抛出 Error。
 */
function assertEqual<T>(actual: T, expected: T, message: string): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`${message}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench Prompt 优化快捷键测试只需要 KeyboardEvent 的少数字段。
 *
 * Code Logic（这个函数做什么）:
 *   构造 reducePromptOptimizerShortcut 可消费的最小事件形状。
 */
function keyEvent(init: {
  type: 'keydown' | 'keyup';
  key: string;
  ctrlKey?: boolean;
  metaKey?: boolean;
  altKey?: boolean;
  shiftKey?: boolean;
  repeat?: boolean;
}) {
  return {
    type: init.type,
    key: init.key,
    ctrlKey: init.ctrlKey ?? false,
    metaKey: init.metaKey ?? false,
    altKey: init.altKey ?? false,
    shiftKey: init.shiftKey ?? false,
    repeat: init.repeat ?? false,
  };
}

assertEqual(
  selectPromptOptimizerInsertText({
    optimizedZh: '中文结果',
    optimizedEn: 'English result',
  }),
  '中文结果',
  'selects Chinese optimized result first',
);

assertEqual(
  selectPromptOptimizerInsertText(
    {
      optimizedZh: '中文结果',
      optimizedEn: 'English result',
    },
    'en',
  ),
  'English result',
  'selects English optimized result when preferred',
);

assertEqual(
  selectPromptOptimizerInsertText(
    {
      optimizedZh: '中文结果',
      optimizedEn: '  ',
    },
    'en',
  ),
  '中文结果',
  'falls back to Chinese when preferred English is empty',
);

assertEqual(
  selectPromptOptimizerInsertText({
    optimizedZh: '   ',
    optimizedEn: 'English result',
  }),
  'English result',
  'falls back to English optimized result',
);

assertEqual(canFillPromptIntoTerminal(session('running')), true, 'running session can fill');
assertEqual(canFillPromptIntoTerminal(null), false, 'missing session cannot fill');
assertEqual(canFillPromptIntoTerminal(session('exited')), false, 'non-running session cannot fill');

assertEqual(
  resetPromptOptimizerTextState({
    input: '上一次输入',
    result: {
      optimizedZh: '上一次中文结果',
      optimizedEn: 'previous English result',
    },
    message: '上一次状态',
  }),
  {
    input: '',
    result: {
      optimizedZh: '',
      optimizedEn: '',
    },
    message: null,
  },
  'opening prompt optimizer resets all visible text',
);

assertEqual(
  promptOptimizerWorkingDirectory(project('/Users/hans/project/Pando')),
  '/Users/hans/project/Pando',
  'uses active project path as Claude working directory',
);

assertEqual(
  promptOptimizerWorkingDirectory(null),
  undefined,
  'missing active project has no working directory',
);

assertEqual(
  promptOptimizerInsertPayload({
    optimizedZh: '中文结果',
    optimizedEn: 'English result',
  }),
  '中文结果',
  'insert payload does not append Enter',
);

assertEqual(
  promptOptimizerInsertPayload(
    {
      optimizedZh: '中文结果',
      optimizedEn: 'English result',
    },
    'en',
  ),
  'English result',
  'insert payload follows preferred language',
);

assertEqual(
  promptOptimizerInsertPayload({
    optimizedZh: '中文结果\n',
    optimizedEn: '',
  }),
  '中文结果\n',
  'insert payload preserves existing trailing newline',
);

assertEqual(
  shouldCommitPromptOptimizerPanelPosition(false, { left: 16, top: 24 }, { left: 40, top: 48 }),
  false,
  'closed panel should not commit cursor position changes',
);

assertEqual(
  shouldCommitPromptOptimizerPanelPosition(true, { left: 16, top: 24 }, { left: 16, top: 24 }),
  false,
  'open panel should skip identical cursor position changes',
);

assertEqual(
  shouldCommitPromptOptimizerPanelPosition(true, { left: 16, top: 24 }, { left: 40, top: 48 }),
  true,
  'open panel should commit changed cursor position',
);

assertEqual(
  promptOptimizerShortcutAction(false, ''),
  'open',
  'shortcut opens panel when it is closed',
);

assertEqual(
  promptOptimizerShortcutAction(true, '   '),
  'close',
  'shortcut closes opened panel when input is empty',
);

assertEqual(
  promptOptimizerShortcutAction(true, '修复工作台'),
  'optimize',
  'shortcut optimizes only when opened panel has input',
);

assertEqual(
  promptOptimizerInputKeyAction({ key: 'Enter', shiftKey: false }, '修复工作台'),
  'optimize',
  'enter optimizes when input has text',
);

assertEqual(
  promptOptimizerInputKeyAction({ key: 'Enter', shiftKey: false }, '   '),
  'ignore',
  'enter ignores empty input',
);

assertEqual(
  promptOptimizerInputKeyAction({ key: 'Enter', shiftKey: true }, '修复工作台'),
  'newline',
  'shift enter keeps multiline editing',
);

assertEqual(
  promptOptimizerInputKeyAction({ key: 'Enter', shiftKey: false, isComposing: true }, '修复工作台'),
  'ignore',
  'enter during IME composition does not optimize',
);

let shortcut = reducePromptOptimizerShortcut(
  createPromptOptimizerShortcutState(),
  keyEvent({ type: 'keydown', key: 'Control', ctrlKey: true }),
  '<ctrl>',
);
assertEqual(shortcut.triggered, false, 'control keydown starts modifier-only tap');
shortcut = reducePromptOptimizerShortcut(
  shortcut.state,
  keyEvent({ type: 'keyup', key: 'Control' }),
  '<ctrl>',
);
assertEqual(shortcut.triggered, true, 'control keyup triggers modifier-only tap');

shortcut = reducePromptOptimizerShortcut(
  createPromptOptimizerShortcutState(),
  keyEvent({ type: 'keydown', key: 'Control', ctrlKey: true }),
  '<ctrl>',
);
shortcut = reducePromptOptimizerShortcut(
  shortcut.state,
  keyEvent({ type: 'keydown', key: 'c', ctrlKey: true }),
  '<ctrl>',
);
shortcut = reducePromptOptimizerShortcut(
  shortcut.state,
  keyEvent({ type: 'keyup', key: 'Control' }),
  '<ctrl>',
);
assertEqual(shortcut.triggered, false, 'control plus another key does not trigger modifier-only tap');

shortcut = reducePromptOptimizerShortcut(
  createPromptOptimizerShortcutState(),
  keyEvent({ type: 'keydown', key: 'p', ctrlKey: true }),
  '<ctrl>+p',
);
assertEqual(shortcut.triggered, true, 'configured combo triggers on keydown');

console.log('promptOptimizerWidget.test.ts passed');
