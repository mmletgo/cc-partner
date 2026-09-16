// @vitest-environment jsdom
/**
 * WorkbenchGitInspector 视觉语义契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   Git 历史采用类似 VS Code Source Control Graph 的紧凑连续泳道后，必须稳定区分 HEAD、merge 与普通提交，
 *   同时保留本地/远端 ref 和可读的提交元信息；合并失败后阶段条必须提供关闭出口。
 *
 * Code Logic（这个测试做什么）:
 *   渲染包含 merge + 双泳道的四条提交，断言行数、节点形态、SVG 泳道、ref 标签与作者/hash 元数据；
 *   另覆盖失败阶段条的关闭按钮显隐与点击回调。
 */

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import type { WorkbenchGitCommit, WorkbenchMergeStage, WorkbenchWorktree } from '@/lib/types';
import { WorkbenchGitInspector } from './WorkbenchGitInspector';
import type { WorkbenchGitInspectorProps } from './WorkbenchGitInspector';

const NOW = '2026-08-13T08:00:00.000Z';

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   组件工具条依赖完整 worktree 状态，但视觉测试不需要执行 mutation。
 *
 * Code Logic（这个函数做什么）:
 *   返回 clean main worktree，确保工具条和分支名可稳定渲染。
 */
function makeWorktree(): WorkbenchWorktree {
  return {
    id: 'project:main',
    projectId: 'project',
    name: 'main',
    branch: 'main',
    baseBranch: null,
    path: '/tmp/project',
    isMain: true,
    canCollectMerge: false,
    homeBranch: null,
    collectibleBranches: [],
    status: {
      branch: 'main',
      changed: 0,
      ahead: 0,
      behind: 0,
      conflicts: 0,
      clean: true,
      canPush: false,
    },
    createdAt: NOW,
    updatedAt: NOW,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   连续泳道和 merge 节点只有在非线性 DAG 下才可验证。
 *
 * Code Logic（这个函数做什么）:
 *   构造 merge → main/feature → base 的四提交拓扑，并在 HEAD 与 feature 上添加 local/remote ref。
 */
function makeCommits(): WorkbenchGitCommit[] {
  return [
    {
      hash: 'merge',
      shortHash: 'a1b2c3d',
      parentHashes: ['main-parent', 'feature-parent'],
      authorName: 'Alice',
      authorEmail: 'alice@example.com',
      authoredAt: NOW,
      summary: 'Merge feature workspace',
      refs: [
        {
          name: 'main',
          fullName: 'refs/heads/main',
          kind: 'local',
          remote: null,
          isHead: true,
        },
      ],
    },
    {
      hash: 'main-parent',
      shortHash: 'b2c3d4e',
      parentHashes: ['base'],
      authorName: 'Alice',
      authorEmail: 'alice@example.com',
      authoredAt: NOW,
      summary: 'Main work',
      refs: [],
    },
    {
      hash: 'feature-parent',
      shortHash: 'c3d4e5f',
      parentHashes: ['base'],
      authorName: 'Bob',
      authorEmail: 'bob@example.com',
      authoredAt: NOW,
      summary: 'Feature work',
      refs: [
        {
          name: 'origin/feature',
          fullName: 'refs/remotes/origin/feature',
          kind: 'remote',
          remote: 'origin',
          isHead: false,
        },
      ],
    },
    {
      hash: 'base',
      shortHash: 'd4e5f6a',
      parentHashes: [],
      authorName: 'Alice',
      authorEmail: 'alice@example.com',
      authoredAt: NOW,
      summary: 'Base commit',
      refs: [],
    },
  ];
}

/**
 * Business Logic（为什么需要这个函数）:
 *   图呈现与失败关闭按钮测试共享同一套 inspector props，避免漏传新增必填回调。
 *
 * Code Logic（这个函数做什么）:
 *   填入默认 Git inspector props，再用 overrides 覆盖 mergeStages / 回调。
 */
function renderGitInspector(
  overrides: Partial<WorkbenchGitInspectorProps> = {},
): ReturnType<typeof render> {
  return render(
    <I18nextProvider i18n={i18n}>
      <WorkbenchGitInspector
        activeProjectId="project"
        activeWorktree={makeWorktree()}
        remoteWriteDisabled={false}
        gitCommits={makeCommits()}
        gitHistoryLoading={false}
        gitHistoryError={null}
        worktreeBusy={null}
        unknownMutationLock={null}
        hookRepair={null}
        handleRepairHookFailure={vi.fn(async () => undefined)}
        handleDismissHookFailure={vi.fn(async () => undefined)}
        handleRetryAfterRepair={vi.fn(async () => undefined)}
        mergeStages={[]}
        clearMergeStagePanel={vi.fn()}
        loadGitHistory={vi.fn(async () => undefined)}
        handleCommitWorktree={vi.fn(async () => undefined)}
        handlePullWorktree={vi.fn(async () => undefined)}
        handlePushWorktree={vi.fn(async () => undefined)}
        handleMergeWorktree={vi.fn(async () => undefined)}
        handleSyncProjectMain={vi.fn(async () => undefined)}
        canSyncProjectMain={false}
        worktreeSyncNotice={null}
        {...overrides}
      />
    </I18nextProvider>,
  );
}

describe('WorkbenchGitInspector graph presentation', () => {
  test('renders compact continuous lanes with distinct HEAD/merge node and inline refs', () => {
    const { container } = renderGitInspector();

    const rows = screen.getAllByTestId('git-history-row');
    expect(rows).toHaveLength(4);
    expect(rows[0]?.getAttribute('data-head')).toBe('true');
    expect(rows[0]?.getAttribute('data-merge')).toBe('true');
    expect(rows[1]?.getAttribute('data-head')).toBeNull();
    expect(container.querySelectorAll('svg path').length).toBeGreaterThan(4);
    expect(container.querySelectorAll('circle').length).toBeGreaterThanOrEqual(5);
    expect(screen.getByTitle('refs/heads/main')).toBeTruthy();
    expect(screen.getByTitle('refs/remotes/origin/feature')).toBeTruthy();
    expect(screen.getByText('Bob')).toBeTruthy();
    expect(screen.getByText('c3d4e5f')).toBeTruthy();
  });

  test('shows the full branch name and version tag in a tooltip on hover', () => {
    const longBranch = 'feat/agent-hub-user-instruction-three-pane-redesign';
    const longTag = 'v0.12.0-beta.1+build.20260916';
    const [mergeCommit, ...rest] = makeCommits();
    if (!mergeCommit) {
      throw new Error('expected merge commit fixture');
    }
    renderGitInspector({
      activeWorktree: {
        ...makeWorktree(),
        branch: longBranch,
      },
      gitCommits: [
        {
          ...mergeCommit,
          refs: [
            {
              name: longBranch,
              fullName: `refs/heads/${longBranch}`,
              kind: 'local',
              remote: null,
              isHead: true,
            },
            {
              name: longTag,
              fullName: `refs/tags/${longTag}`,
              kind: 'tag',
              remote: null,
              isHead: false,
            },
          ],
        },
        ...rest,
      ],
    });

    expect(screen.queryByRole('tooltip')).toBeNull();

    const branchBadges = screen.getAllByText(longBranch);
    const refBadge = branchBadges.find((node) => node.getAttribute('data-kind') === 'local');
    expect(refBadge).toBeTruthy();
    fireEvent.mouseEnter(refBadge as HTMLElement);
    expect(screen.getByRole('tooltip').textContent).toBe(longBranch);

    fireEvent.mouseLeave(refBadge as HTMLElement);
    expect(screen.queryByRole('tooltip')).toBeNull();

    const tagBadge = screen.getByTitle(`refs/tags/${longTag}`);
    fireEvent.mouseEnter(tagBadge);
    expect(screen.getByRole('tooltip').textContent).toBe(longTag);

    fireEvent.mouseLeave(tagBadge);
    expect(screen.queryByRole('tooltip')).toBeNull();

    const currentBranch = branchBadges.find((node) => node.className.includes('gitActionBranch'));
    expect(currentBranch).toBeTruthy();
    fireEvent.mouseEnter(currentBranch as HTMLElement);
    expect(screen.getByRole('tooltip').textContent).toBe(longBranch);
  });

  test('renders Pull and Push on the status row between Clean and the branch name', () => {
    renderGitInspector();
    const pull = screen.getByRole('button', { name: 'Pull' });
    const push = screen.getByRole('button', { name: 'Push' });
    const commit = screen.getByRole('button', { name: 'Commit' });
    const merge = screen.getByRole('button', { name: '合并' });

    const statusRow = pull.parentElement?.parentElement;
    expect(statusRow?.textContent).toContain('Clean');
    expect(statusRow?.textContent).toContain('main');
    expect(statusRow?.contains(pull)).toBe(true);
    expect(statusRow?.contains(push)).toBe(true);
    expect(statusRow?.contains(commit)).toBe(false);
    expect(statusRow?.contains(merge)).toBe(false);
  });

  test('places sync button to the right of commit', () => {
    const onSync = vi.fn(async () => undefined);
    renderGitInspector({
      canSyncProjectMain: true,
      handleSyncProjectMain: onSync,
    });
    const commit = screen.getByRole('button', { name: 'Commit' });
    const sync = screen.getByTestId('workbench-git-sync');
    expect(commit.nextElementSibling).toBe(sync);
    expect(sync.textContent).toContain('同步');
    expect(sync.hasAttribute('disabled')).toBe(false);
    fireEvent.click(sync);
    expect(onSync).toHaveBeenCalledTimes(1);
  });

  test('disables sync when canSyncProjectMain is false', () => {
    renderGitInspector({ canSyncProjectMain: false });
    expect(screen.getByTestId('workbench-git-sync').hasAttribute('disabled')).toBe(true);
  });
});

describe('WorkbenchGitInspector merge stage dismiss', () => {
  test('shows dismiss control after mergeMain fails and later stages stay pending', () => {
    const onDismiss = vi.fn();
    const mergeStages: WorkbenchMergeStage[] = [
      { id: 'checkSource', status: 'completed', message: '源 worktree 已确认干净' },
      { id: 'closeSessions', status: 'completed', message: '已关闭 1 个终端窗口' },
      {
        id: 'mergeMain',
        status: 'failed',
        message: '主工作区有未提交改动，请先提交或清理后再合并',
      },
      { id: 'resolveConflicts', status: 'pending', message: '' },
      { id: 'cleanup', status: 'pending', message: '' },
    ];

    renderGitInspector({ mergeStages, clearMergeStagePanel: onDismiss });

    const dismiss = screen.getByTestId('workbench-merge-stages-dismiss');
    expect(dismiss).toBeTruthy();
    fireEvent.click(dismiss);
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  test('hides dismiss control while a merge stage is still running', () => {
    const mergeStages: WorkbenchMergeStage[] = [
      { id: 'checkSource', status: 'completed', message: '源 worktree 已确认干净' },
      { id: 'closeSessions', status: 'running', message: '正在关闭终端' },
    ];

    renderGitInspector({ mergeStages });

    expect(screen.queryByTestId('workbench-merge-stages-dismiss')).toBeNull();
  });

  test('shows one Claude Code activity line under a running resolveConflicts status', () => {
    const mergeStages: WorkbenchMergeStage[] = [
      { id: 'checkSource', status: 'completed', message: '源 worktree 已确认干净' },
      { id: 'closeSessions', status: 'completed', message: '已关闭终端' },
      { id: 'mergeMain', status: 'completed', message: '隔离 merge 出现冲突，进入自动解决阶段' },
      {
        id: 'resolveConflicts',
        status: 'running',
        message: '正在调用 Claude Code 尝试解决 merge 冲突',
        activity: 'Read src/lib.rs',
      },
      { id: 'cleanup', status: 'pending', message: '' },
    ];

    renderGitInspector({ mergeStages });

    expect(screen.getByText('正在调用 Claude Code 尝试解决 merge 冲突')).toBeTruthy();
    const activity = screen.getByTestId('workbench-merge-stage-activity');
    expect(activity.textContent).toBe('Read src/lib.rs');
    expect(activity.closest('[data-status="running"]')).toBeTruthy();
  });

  test('does not show activity after resolveConflicts leaves running', () => {
    const mergeStages: WorkbenchMergeStage[] = [
      {
        id: 'resolveConflicts',
        status: 'completed',
        message: 'Claude Code 已在隔离目录解决冲突并完成 merge commit',
        activity: 'Read src/lib.rs',
      },
    ];

    renderGitInspector({ mergeStages });

    expect(screen.queryByTestId('workbench-merge-stage-activity')).toBeNull();
  });
});
