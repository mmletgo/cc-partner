// @vitest-environment jsdom
/**
 * Workbench 项目域 characterization 测试。
 *
 * Business Logic: 在 controller 抽取前锁住项目切换、stale 项目响应丢弃、远端离线→写禁用→恢复 这些
 * 当前用户可观察行为；后续重构必须保持这些断言为绿。
 *
 * Code Logic: 使用 workbenchTestHarness 的 stub context + fake invoke 控制时序；断言 DOM 文案、按钮
 * disabled 状态、invoke 调用日志，避免触碰实现细节。
 */
import { describe, expect, test } from 'vitest';
import { fireEvent, screen } from '@testing-library/react';

import {
  buildDependencyContextValue,
  buildLocalProject,
  buildProjectsContextValue,
  buildRemoteProject,
  buildSession,
  buildWorktree,
  createDeferred,
  flushMacrotasks,
  invokeCallsFor,
  renderWorkbench,
  setInvokeHandler,
} from './testing/workbenchTestHarness';
import type { WorkbenchWorktree } from '@/lib/types';

const REMOTE_OFFLINE_ERROR = '远端设备不在线';

async function settle(): Promise<void> {
  await flushMacrotasks();
}

describe('Workbench project domain (characterization)', () => {
  test('existing projects with no selection render continue-working launch surface', async () => {
    const project = buildLocalProject({ name: 'demo-project' });
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'get_workbench_launch_summary':
          return {
            projects: {
              kind: 'ready',
              value: [
                {
                  id: project.id,
                  name: project.name,
                  kind: project.kind,
                  deviceId: project.deviceId,
                  deviceName: project.deviceName,
                  path: project.path,
                  lastOpenedAt: project.lastOpenedAt,
                },
              ],
            },
            sessions: { kind: 'ready', value: [] },
            tasks: { kind: 'ready', value: [] },
            transfers: { kind: 'ready', value: [] },
            devices: { kind: 'ready', value: [] },
            generatedAt: '2026-07-14T12:00:00.000Z',
          };
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: null }),
      buildDependencyContextValue(),
    );
    await settle();

    expect(screen.getByTestId('workbench-launch-continue')).toBeTruthy();
    expect(screen.getByText('继续工作')).toBeTruthy();
    expect(screen.queryByRole('region', { name: 'Worktree 管理' })).toBeNull();
    expect(invokeCallsFor('get_workbench_launch_summary').length).toBeGreaterThan(0);
  });

  test('zero projects render only three focused empty actions', async () => {
    setInvokeHandler(() => ({ ok: true }));
    renderWorkbench(
      buildProjectsContextValue({ projects: [], activeProjectId: null }),
      buildDependencyContextValue(),
    );
    await settle();

    expect(screen.getByTestId('workbench-launch-empty')).toBeTruthy();
    expect(screen.getByRole('button', { name: '添加本机项目' })).toBeTruthy();
    expect(screen.getByRole('button', { name: '连接远端项目' })).toBeTruthy();
    expect(screen.getByRole('button', { name: '检查 tmux 依赖' })).toBeTruthy();
    expect(screen.queryByTestId('workbench-launch-continue')).toBeNull();
    expect(screen.queryByRole('region', { name: 'Worktree 管理' })).toBeNull();
    expect(invokeCallsFor('get_workbench_launch_summary')).toHaveLength(0);
  });

  test('projectsLoading does not render empty or continue launch surface', async () => {
    // M1: load 中 projects=[] 不得假零项目 CTA，也不得误入 continue。
    setInvokeHandler(() => ({ ok: true }));
    renderWorkbench(
      buildProjectsContextValue(
        { projects: [], activeProjectId: 'stored-project-id' },
        { projectsLoading: true },
      ),
      buildDependencyContextValue(),
    );
    await settle();

    expect(screen.getByTestId('workbench-projects-loading')).toBeTruthy();
    expect(screen.getByText('加载中')).toBeTruthy();
    expect(screen.queryByTestId('workbench-launch-empty')).toBeNull();
    expect(screen.queryByTestId('workbench-launch-continue')).toBeNull();
    expect(screen.queryByRole('button', { name: '添加本机项目' })).toBeNull();
    expect(screen.queryByRole('region', { name: 'Worktree 管理' })).toBeNull();
    expect(invokeCallsFor('get_workbench_launch_summary')).toHaveLength(0);
  });

  test('no project shows focused actions without terminal chrome', async () => {
    setInvokeHandler(() => ({ ok: true }));
    renderWorkbench(
      buildProjectsContextValue({ projects: [], activeProjectId: null }),
      buildDependencyContextValue(),
    );
    await settle();

    // Design §5：零项目只保留三个聚焦 CTA + 一句解释，不渲染禁用 toolbar / 空终端 / inspector。
    expect(screen.getByRole('button', { name: '添加本机项目' })).toBeTruthy();
    expect(screen.getByRole('button', { name: '连接远端项目' })).toBeTruthy();
    expect(screen.getByRole('button', { name: '检查 tmux 依赖' })).toBeTruthy();
    expect(
      screen.getByText('项目添加后可管理终端、文件、Git 与自动化'),
    ).toBeTruthy();
    expect(screen.queryByTestId('terminal-pane')).toBeNull();
    expect(screen.queryByTestId('workbench-inspector')).toBeNull();
    expect(screen.queryByRole('region', { name: 'Worktree 管理' })).toBeNull();
    expect(screen.queryByRole('navigation', { name: '终端会话' })).toBeNull();
    expect(screen.queryByRole('button', { name: '文件预览' })).toBeNull();
  });

  test('active project still renders normal Workbench chrome', async () => {
    const project = buildLocalProject({ name: 'demo-project' });
    const worktree = buildWorktree({ id: 'wt-main', name: 'main', branch: 'main' });
    const session = buildSession({ id: 's1', name: 'main shell' });
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return [worktree];
        case 'list_workbench_sessions':
          return [session];
        case 'list_workbench_git_commits':
          return [];
        case 'list_workbench_dir':
          return [];
        case 'get_workbench_launch_summary':
          throw new Error('launch should not be fetched for active project');
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [project], activeProjectId: project.id }),
      buildDependencyContextValue(),
    );
    await settle();

    expect(screen.queryByTestId('workbench-launch-continue')).toBeNull();
    expect(screen.queryByTestId('workbench-launch-empty')).toBeNull();
    expect(screen.getByText(`${project.deviceName} · ${project.path}`)).toBeTruthy();
    expect(screen.getAllByTestId('terminal-pane').length).toBeGreaterThan(0);
    expect(screen.getByTestId('workbench-inspector')).toBeTruthy();
    expect(invokeCallsFor('get_workbench_launch_summary')).toHaveLength(0);
  });

  test('loads worktrees and sessions for the active project on mount', async () => {
    const project = buildLocalProject({ name: 'demo-project' });
    const worktree = buildWorktree({ id: 'wt-main', name: 'main', branch: 'main' });
    const session = buildSession({ id: 's1', name: 'main shell' });
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_projects':
          return [project];
        case 'list_workbench_worktrees':
          return [worktree];
        case 'list_workbench_sessions':
          return [session];
        case 'list_workbench_git_commits':
          return [];
        case 'list_workbench_dir':
          return [];
        default:
          return { ok: true };
      }
    });

    const projectsValue = buildProjectsContextValue({
      projects: [project],
      activeProjectId: project.id,
    });
    renderWorkbench(projectsValue, buildDependencyContextValue());
    await settle();

    expect(invokeCallsFor('list_workbench_worktrees').length).toBeGreaterThan(0);
    expect(invokeCallsFor('list_workbench_sessions').length).toBeGreaterThan(0);
    // 工作区路径行展示当前项目 deviceName · path。
    expect(screen.getByText(`${project.deviceName} · ${project.path}`)).toBeTruthy();
    // 终端 tab 显示当前 session 名称。
    expect(screen.getByText('main shell')).toBeTruthy();
  });

  test('switching active project reloads worktrees and sessions for the new project', async () => {
    const projectA = buildLocalProject({ id: 'pA', name: 'project-a' });
    const projectB = buildLocalProject({ id: 'pB', name: 'project-b' });
    const wtA = buildWorktree({
      id: 'wtA',
      projectId: 'pA',
      name: 'main-a',
      branch: 'main-a',
    });
    const wtB = buildWorktree({
      id: 'wtB',
      projectId: 'pB',
      name: 'main-b',
      branch: 'main-b',
    });
    const sessionA = buildSession({
      id: 'sA',
      projectId: 'pA',
      worktreeId: 'wtA',
      name: 'shell-a',
    });

    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_worktrees':
          return call.args.projectId === 'pA' ? [wtA] : [wtB];
        case 'list_workbench_sessions':
          return call.args.projectId === 'pA' ? [sessionA] : [];
        case 'list_workbench_git_commits':
          return [];
        case 'list_workbench_dir':
          return [];
        default:
          return { ok: true };
      }
    });

    const projectsValueA = buildProjectsContextValue({
      projects: [projectA, projectB],
      activeProjectId: 'pA',
    });
    const utils = renderWorkbench(projectsValueA, buildDependencyContextValue());
    await settle();
    expect(screen.getAllByText('shell-a').length).toBeGreaterThan(0);

    // 切到 B：context value 变化触发 effect 重新 loadWorktrees / loadSessions。
    utils.setProjectsContext(
      buildProjectsContextValue({ projects: [projectA, projectB], activeProjectId: 'pB' }),
    );
    await settle();

    // session shell-a 应消失（B 项目无 session）；worktree chip 切换为 main-b。
    expect(screen.queryAllByText('shell-a')).toHaveLength(0);
    const worktreeBar = screen.getByRole('region', { name: 'Worktree 管理' });
    expect(worktreeBar.textContent).toContain('main-b');
  });

  test('stale worktree list response is ignored when active project changes', async () => {
    const projectA = buildLocalProject({ id: 'pA', name: 'project-a' });
    const projectB = buildLocalProject({ id: 'pB', name: 'project-b' });
    const wtA: WorkbenchWorktree = buildWorktree({
      id: 'wtA',
      projectId: 'pA',
      name: 'main-a',
      branch: 'main-a',
    });
    const wtB: WorkbenchWorktree = buildWorktree({
      id: 'wtB',
      projectId: 'pB',
      name: 'main-b',
      branch: 'main-b',
    });

    // pA 的 list_workbench_worktrees 返回 deferred；切到 B 后才 resolve。
    const worktreesDeferred = createDeferred<WorkbenchWorktree[]>();
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_worktrees':
          if (call.args.projectId === 'pA') return worktreesDeferred.promise;
          return [wtB];
        case 'list_workbench_sessions':
          return [];
        case 'list_workbench_git_commits':
          return [];
        case 'list_workbench_dir':
          return [];
        default:
          return { ok: true };
      }
    });

    const utils = renderWorkbench(
      buildProjectsContextValue({ projects: [projectA, projectB], activeProjectId: 'pA' }),
      buildDependencyContextValue(),
    );
    await settle();

    // 切到 B：B 的 loadWorktrees 立即返回 [wtB]。
    utils.setProjectsContext(
      buildProjectsContextValue({ projects: [projectA, projectB], activeProjectId: 'pB' }),
    );
    await settle();

    // 现在 resolve pA 的 stale deferred；按 stale guard，[wtA] 不应写入 worktree 列表。
    worktreesDeferred.resolve([wtA]);
    await settle();

    const worktreeBar = screen.getByRole('region', { name: 'Worktree 管理' });
    expect(worktreeBar.textContent).not.toContain('main-a');
    expect(worktreeBar.textContent).toContain('main-b');
  });

  test('remote offline error disables writes and shows offline notice; later success restores', async () => {
    const remote = buildRemoteProject({ id: 'pRemote', name: 'remote' });
    const wt = buildWorktree({
      id: 'wtRemote',
      projectId: 'pRemote',
      name: 'main-remote',
      branch: 'main-remote',
    });

    // 第一轮所有远端读命令都抛远端离线错误（worktrees + sessions + git + dir），
    // 这样 remoteOfflineProjectId 才不会被随后成功的 sessions 命令立即清掉。
    // 切到 Git 历史 tab 后，第二轮 list_workbench_git_commits 成功 → 触发 clearRemoteOfflineForProject。
    let offline = true;
    setInvokeHandler((call) => {
      switch (call.cmd) {
        case 'list_workbench_worktrees':
          return offline ? Promise.reject(new Error(REMOTE_OFFLINE_ERROR)) : [wt];
        case 'list_workbench_sessions':
          return offline ? Promise.reject(new Error(REMOTE_OFFLINE_ERROR)) : [];
        case 'list_workbench_git_commits':
          return offline ? Promise.reject(new Error(REMOTE_OFFLINE_ERROR)) : [];
        case 'list_workbench_dir':
          return offline ? Promise.reject(new Error(REMOTE_OFFLINE_ERROR)) : [];
        case 'touch_workbench_project':
          return remote;
        default:
          return { ok: true };
      }
    });

    renderWorkbench(
      buildProjectsContextValue({ projects: [remote], activeProjectId: remote.id }),
      buildDependencyContextValue(),
    );
    await settle();

    // 远端离线提示（notice 文案含“当前不在线”）出现；写操作（创建 worktree）应被禁用。
    expect(screen.getByText(/当前不在线/)).toBeTruthy();
    const createButton = screen.getByRole('button', { name: '新建 worktree' });
    expect(createButton.hasAttribute('disabled')).toBe(true);

    // 恢复在线：让后续请求成功，并触发一次远端读命令（Git 历史 tab → list_workbench_git_commits）。
    offline = false;
    fireEvent.click(screen.getByRole('tab', { name: 'Git 历史' }));
    await settle();

    // 离线提示消失，写操作恢复可用。
    expect(screen.queryByText(/当前不在线/)).toBeNull();
    const createButtonAfter = screen.getByRole('button', { name: '新建 worktree' });
    expect(createButtonAfter.hasAttribute('disabled')).toBe(false);
  });
});
