import { describe, test, expect } from 'vitest';
import { readFileSync } from 'node:fs';

/**
 * Business Logic（为什么需要这个测试文件）:
 *   Plan 2 Task 8 把 Workbench.tsx 的检查器渲染（文件树/path info 与 Git 历史/actions）抽到独立的
 *   WorkbenchInspector / WorkbenchFileInspector / WorkbenchGitInspector 子组件，并要求页面文件降到 ≤1200 行。
 *   重构是行为保持型，但页面源码形态会变化（不再直接调用 workbenchApi、不再订阅 terminal/merge 事件、
 *   不再持有名为 workbenchController 的页面级 state 对象）。本测试用静态源码扫描锁住这些不变式，
 *   防止后续回归把领域 state/事件订阅再塞回页面文件。
 *
 * Code Logic（这个测试文件做什么）:
 *   - 读取 Workbench.tsx 源码，断言它不再直接调用 workbenchApi.sessions/files/worktrees/git 的方法；
 *   - 断言不再直接订阅 terminal-output / terminal-status / merge-progress / worktree-* 等 Tauri 事件；
 *   - 断言不再持有名为 workbenchController 的页面级 state 对象（控制器分散到各 useXxxController）；
 *   - 断言总行数 ≤ 1200。
 */

const WORKBENCH_PATH = new URL('./Workbench.tsx', import.meta.url);
const workbenchSource = readFileSync(WORKBENCH_PATH, 'utf8');
const workbenchLineCount = workbenchSource.split('\n').length;

describe('Workbench.tsx source ownership (Task 8 inspector extraction)', () => {
  test('does not import or call workbenchApi directly', () => {
    // 控制器层已接管 workbenchApi 调用；页面只通过 controller 暴露的 handler 与后端通信。
    expect(workbenchSource).not.toContain('workbenchApi.sessions.');
    expect(workbenchSource).not.toContain('workbenchApi.files.');
    expect(workbenchSource).not.toContain('workbenchApi.worktrees.');
    expect(workbenchSource).not.toContain('workbenchApi.git.');
    // 即便 import 残留也应移除（页面不再使用）。
    expect(workbenchSource).not.toMatch(/import\s*\{[^}]*\bworkbenchApi\b[^}]*\}\s*from\s*'@\/api\/workbench'/);
  });

  test('does not directly subscribe to terminal / merge / worktree Tauri events', () => {
    // 事件订阅归控制器层；页面只通过 controller 暴露的 state/handler 间接消费事件结果。
    expect(workbenchSource).not.toMatch(/listen\(\s*['"]terminal-output['"]/);
    expect(workbenchSource).not.toMatch(/listen\(\s*['"]terminal-status['"]/);
    expect(workbenchSource).not.toMatch(/listen\(\s*['"]merge-progress['"]/);
    expect(workbenchSource).not.toMatch(/listen\(\s*['"]worktree-changed['"]/);
    expect(workbenchSource).not.toMatch(/listen\(\s*['"]worktree-status['"]/);
  });

  test('does not declare a page-level state object named workbenchController', () => {
    // 控制器被拆分成多个 useWorkbenchXxxController hook；页面不应再聚合出单一巨型 controller state。
    expect(workbenchSource).not.toMatch(/\bconst\s+workbenchController\b/);
    expect(workbenchSource).not.toMatch(/\buseState\b[^;]*\bworkbenchController\b/);
  });

  test('stays within the 1200-line target', () => {
    expect(workbenchLineCount).toBeLessThanOrEqual(1200);
  });

  test('delegates inspector rendering to WorkbenchInspector', () => {
    // 页面只组合检查器外壳；具体 tabs / 文件树 / Git 图由子组件渲染。
    expect(workbenchSource).toContain('WorkbenchInspector');
  });
});
