// @vitest-environment jsdom
/**
 * WorktreeBar hint 数字测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   worktree 左点有 hint 时必须放大写数字，且不丢掉 Git tone。
 *
 * Code Logic（这个测试做什么）:
 *   注入 hint Context，断言 chip 左点数字与 aria。
 */

import { afterEach, describe, expect, test, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import { createRef } from 'react';
import i18n from '@/i18n';
import { WorkbenchAgentHintsContext } from '@/hooks/workbenchAgentHintsContext';
import { EMPTY_HINT_COUNTS } from '@/lib/workbenchAgentHints';
import type { WorkbenchWorktree } from '@/lib/types';
import { WorkbenchWorktreeBar } from './WorkbenchWorktreeBar';

const worktree: WorkbenchWorktree = {
  id: 'wt-1',
  projectId: 'p1',
  name: 'feature/wait',
  path: '/tmp/wt',
  branch: 'feature/wait',
  baseBranch: 'main',
  isMain: false,
  canCollectMerge: false,
  homeBranch: null,
  collectibleBranches: [],
  createdAt: '2026-08-13T00:00:00.000Z',
  updatedAt: '2026-08-13T00:00:00.000Z',
  status: {
    branch: 'feature/wait',
    changed: 0,
    clean: true,
    ahead: 0,
    behind: 0,
    conflicts: 0,
    canPush: false,
  },
};

describe('WorkbenchWorktreeBar hints', () => {
  afterEach(() => {
    cleanup();
  });

  test('shows waiting count on worktree dot', async () => {
    await i18n.changeLanguage('zh');
    render(
      <I18nextProvider i18n={i18n}>
        <WorkbenchAgentHintsContext.Provider
          value={{
            phase: 'live',
            error: null,
            hintsForProject: () => EMPTY_HINT_COUNTS,
            hintsForWorktree: () => ({
              waitingCount: 1,
              stoppedCount: 1,
              completedCount: 1,
              count: 2,
              tone: 'wait',
            }),
            hintsForTerminal: () => EMPTY_HINT_COUNTS,
            ackCompletedForTerminal: () => undefined,
            refresh: async () => undefined,
          }}
        >
          <WorkbenchWorktreeBar
            worktrees={[worktree]}
            activeWorktree={worktree}
            activeProjectId="p1"
            remoteWriteDisabled={false}
            worktreeBusy={null}
            unknownMutationLock={null}
            createWorktreeOpen={false}
            createWorktreeBranchPrefix="feature"
            createWorktreeBranchSuffixDraft=""
            worktreeBranchInputRef={createRef<HTMLInputElement>()}
            onSelectWorktree={vi.fn()}
            setCreateWorktreeBranchPrefix={vi.fn()}
            setCreateWorktreeBranchSuffixDraft={vi.fn()}
            handleOpenCreateWorktree={vi.fn()}
            handleCancelCreateWorktree={vi.fn()}
            handleCreateWorktree={async () => undefined}
            handleRemoveWorktree={async () => undefined}
          />
        </WorkbenchAgentHintsContext.Provider>
      </I18nextProvider>,
    );
    expect(screen.getByLabelText('1 个窗口等待输入，1 个窗口已停止').textContent).toBe('1/1');
  });
});
