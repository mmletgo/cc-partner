// @vitest-environment jsdom
/**
 * WorkbenchGitInspector 视觉语义契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   Git 历史采用类似 VS Code Source Control Graph 的紧凑连续泳道后，必须稳定区分 HEAD、merge 与普通提交，
 *   同时保留本地/远端 ref 和可读的提交元信息。
 *
 * Code Logic（这个测试做什么）:
 *   渲染包含 merge + 双泳道的四条提交，断言行数、节点形态、SVG 泳道、ref 标签与作者/hash 元数据。
 */

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import type { WorkbenchGitCommit, WorkbenchWorktree } from '@/lib/types';
import { WorkbenchGitInspector } from './WorkbenchGitInspector';

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

describe('WorkbenchGitInspector graph presentation', () => {
  test('renders compact continuous lanes with distinct HEAD/merge node and inline refs', () => {
    const { container } = render(
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
          loadGitHistory={vi.fn(async () => undefined)}
          handleCommitWorktree={vi.fn(async () => undefined)}
          handlePushWorktree={vi.fn(async () => undefined)}
          handleMergeWorktree={vi.fn(async () => undefined)}
        />
      </I18nextProvider>,
    );

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
});
