import { describe, test } from 'vitest';
import {
  getMobileFileContextKey,
  getMobileWorktreeMergeAppliedState,
  isMobileFileOpenResponseCurrent,
  isMobileFileSaveResponseCurrent,
  isMobileGitActionResponseCurrent,
  isMobileGitMergeResponseCurrent,
  isMobileMutationActionLocked,
  pickMobileMutationOperationId,
  resolveMobileMutationPhase,
  shouldBlockMobileFileContextSwitch,
  shouldConfirmMobileFileDirtyContextSwitch,
  shouldSkipMobileFileContextConfirmForDiscardToken,
  shouldInvalidateMobileFileOpenOnDirectoryLoad,
  getMobileWorktreeRemovalPlan,
  getMobileWorktreeMergePlan,
  runMobileWorktreeMergeFlow,
  runMobileWorktreeRefreshFlow,
  runMobileWorktreeRemovalFlow,
  shouldReloadMobileGitCommitsAfterAction,
  shouldSkipMobileFileContextReload,
  type MobileFilePanelContext,
  type MobileFileDirtySnapshot,
} from './mobilePanelState';
import type { WorkbenchWorktree } from '@/lib/types';

/**
 * Business Logic（为什么需要这个函数）:
 *   当前 web tsconfig 会编译 src 下测试文件，但未启用 Node 类型；测试断言需要避免依赖 node:assert。
 *
 * Code Logic（这个函数做什么）:
 *   比较 actual 与 expected，不一致时抛出 Error 让用例失败。
 */
function assertEqual<T>(actual: T, expected: T, message: string): void {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${String(expected)}, received ${String(actual)}`);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   destructive worktree 流程测试需要断言异步后端失败会继续向调用方抛出，避免组件误判成功并丢草稿。
 *
 * Code Logic（这个函数做什么）:
 *   执行 promise factory；若没有抛错则失败，若抛错则断言错误消息与预期一致。
 */
async function assertRejects(
  run: () => Promise<unknown>,
  expectedMessage: string,
  message: string,
): Promise<void> {
  try {
    await run();
  } catch (reason) {
    assertEqual(
      reason instanceof Error ? reason.message : String(reason),
      expectedMessage,
      message,
    );
    return;
  }
  throw new Error(`${message}: expected rejection`);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端文件面板的测试需要构造 project/worktree 上下文，避免在断言中重复字面量。
 *
 * Code Logic（这个函数做什么）:
 *   接收项目与 worktree id，返回 MobileFilePanelContext；空 worktree 统一为 null。
 */
function createContext(projectId: string, worktreeId: string | null): MobileFilePanelContext {
  return { projectId, worktreeId };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 worktree 删除测试需要构造完整 DTO，避免测试断言依赖不完整对象。
 *
 * Code Logic（这个函数做什么）:
 *   接收 worktree id 与主工作区标记，返回带干净 Git 状态的 WorkbenchWorktree。
 */
function createWorktree(id: string, isMain: boolean): WorkbenchWorktree {
  return {
    id,
    projectId: 'project-1',
    name: id,
    branch: isMain ? 'main' : id,
    baseBranch: isMain ? null : 'main',
    path: `/repo/${id}`,
    isMain,
    canCollectMerge: false,
    homeBranch: null,
    collectibleBranches: [],
    status: {
      branch: isMain ? 'main' : id,
      changed: 0,
      ahead: 0,
      behind: 0,
      conflicts: 0,
      clean: true,
      canPush: false,
    },
    createdAt: '2026-01-01T00:00:00Z',
    updatedAt: '2026-01-01T00:00:00Z',
  };
}

describe('mobilePanelState', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   用户取消切换后再回到原 project/worktree 时，未保存草稿必须保留，不能重新加载根目录覆盖当前文件状态。
   *
   * Code Logic（这个测试做什么）:
   *   构造 loaded/next/opened 都相同的上下文，断言上下文同步应跳过 reset 与 reload。
   */
  test('shouldSkipMobileFileContextReload returns to loaded dirty context', () => {
    const context = createContext('project-1', 'worktree-1');

    assertEqual(
      shouldSkipMobileFileContextReload(context, context, context),
      true,
      'returning to loaded dirty context should skip reload',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   dirty 草稿只应阻止真正切换到另一个 project/worktree，同上下文重渲染不应进入阻塞态。
   *
   * Code Logic（这个测试做什么）:
   *   分别断言 dirty+不同上下文会阻塞，dirty+同上下文或 clean+不同上下文不会阻塞。
   */
  test('shouldBlockMobileFileContextSwitch boundary cases', () => {
    const current = createContext('project-1', 'worktree-1');
    const next = createContext('project-1', 'worktree-2');

    assertEqual(
      shouldBlockMobileFileContextSwitch(current, next, true),
      true,
      'dirty changed context should block until user confirms',
    );
    assertEqual(
      shouldBlockMobileFileContextSwitch(current, current, true),
      false,
      'dirty same context should not block',
    );
    assertEqual(
      shouldBlockMobileFileContextSwitch(current, next, false),
      false,
      'clean changed context should not block',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   Files 面板被隐藏但仍挂载时，父级在切换项目或 worktree 前必须能用 dirty snapshot 判断是否需要确认。
   *
   * Code Logic（这个测试做什么）:
   *   构造 dirty snapshot 与不同目标上下文，断言父级切换守卫需要弹出确认。
   */
  test('shouldConfirmMobileFileDirtyContextSwitch requires parent confirm on different target', () => {
    const snapshot: MobileFileDirtySnapshot = {
      dirty: true,
      context: createContext('project-1', 'worktree-1'),
    };
    const next = createContext('project-1', 'worktree-2');

    assertEqual(
      shouldConfirmMobileFileDirtyContextSwitch(snapshot, next),
      true,
      'dirty snapshot should require parent confirm when target context differs',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   用户重新选择当前 project/worktree 时不能误弹确认或清空草稿，否则会破坏 Task8 的“切回原上下文保留草稿”要求。
   *
   * Code Logic（这个测试做什么）:
   *   构造 dirty snapshot 与相同目标上下文，断言父级切换守卫不需要确认。
   */
  test('shouldConfirmMobileFileDirtyContextSwitch skips parent confirm on same target', () => {
    const context = createContext('project-1', 'worktree-1');
    const snapshot: MobileFileDirtySnapshot = {
      dirty: true,
      context,
    };

    assertEqual(
      shouldConfirmMobileFileDirtyContextSwitch(snapshot, context),
      false,
      'dirty snapshot should not require parent confirm when target context is unchanged',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   父级已经确认丢弃 dirty 草稿后，Files 面板响应 props context 变化时不能再次弹出同一条确认。
   *
   * Code Logic（这个测试做什么）:
   *   比较上一次与当前 discard token，断言 token 变化时内部 context confirm 应被跳过，token 不变时不跳过。
   */
  test('shouldSkipMobileFileContextConfirmForDiscardToken tracks token changes', () => {
    assertEqual(
      shouldSkipMobileFileContextConfirmForDiscardToken(1, 2),
      true,
      'changed discard token should skip internal context confirm',
    );
    assertEqual(
      shouldSkipMobileFileContextConfirmForDiscardToken(2, 2),
      false,
      'unchanged discard token should keep normal internal confirm behavior',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   旧 files.open 响应不能在 project/worktree 切换或根目录重载后写入旧文件预览。
   *
   * Code Logic（这个测试做什么）:
   *   同时校验 request id 和发起请求时的 context；任一不匹配都应判定为 stale。
   */
  test('isMobileFileOpenResponseCurrent requires latest request and current context', () => {
    const context = createContext('project-1', 'worktree-1');
    const otherContext = createContext('project-1', 'worktree-2');

    assertEqual(
      isMobileFileOpenResponseCurrent(2, 2, context, context),
      true,
      'latest open response in loaded context should be current',
    );
    assertEqual(
      isMobileFileOpenResponseCurrent(1, 2, context, context),
      false,
      'older open response should be stale',
    );
    assertEqual(
      isMobileFileOpenResponseCurrent(2, 2, context, otherContext),
      false,
      'open response from previous context should be stale',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   保存文本文件期间用户可能切换文件或 worktree，旧 save 响应不能清掉新草稿的 dirty 状态。
   *
   * Code Logic（这个测试做什么）:
   *   同时校验 request id、context key 和文件 path，任一变化都应判定为 stale。
   */
  test('isMobileFileSaveResponseCurrent requires latest context and path', () => {
    const context = createContext('project-1', 'worktree-1');
    const otherContext = createContext('project-1', 'worktree-2');

    assertEqual(
      isMobileFileSaveResponseCurrent(3, 3, context, context, 'src/a.ts', 'src/a.ts'),
      true,
      'latest save response for same context/path should be current',
    );
    assertEqual(
      isMobileFileSaveResponseCurrent(2, 3, context, context, 'src/a.ts', 'src/a.ts'),
      false,
      'older save response should be stale',
    );
    assertEqual(
      isMobileFileSaveResponseCurrent(3, 3, context, otherContext, 'src/a.ts', 'src/a.ts'),
      false,
      'save response from previous context should be stale',
    );
    assertEqual(
      isMobileFileSaveResponseCurrent(3, 3, context, context, 'src/a.ts', 'src/b.ts'),
      false,
      'save response for previous opened file should be stale',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   重新加载根目录代表文件面板回到新的上下文基线，未完成的 open 请求必须失效。
   *
   * Code Logic（这个测试做什么）:
   *   断言空路径根目录需要失效 open 请求，子目录加载不触发该边界规则。
   */
  test('shouldInvalidateMobileFileOpenOnDirectoryLoad only for root directory', () => {
    assertEqual(
      shouldInvalidateMobileFileOpenOnDirectoryLoad(''),
      true,
      'root directory load should invalidate pending open requests',
    );
    assertEqual(
      shouldInvalidateMobileFileOpenOnDirectoryLoad('src'),
      false,
      'child directory load should not be treated as root context reload',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   merge 成功后源 worktree 可能已被后端删除，移动端不能再用旧源 worktree id 拉 commits 并误报失败。
   *
   * Code Logic（这个测试做什么）:
   *   断言 commit/push 成功后仍刷新 commits，但 merge 成功后只刷新 worktrees。
   */
  test('shouldReloadMobileGitCommitsAfterAction skips merge only', () => {
    assertEqual(
      shouldReloadMobileGitCommitsAfterAction('commit'),
      true,
      'commit should reload commits',
    );
    assertEqual(
      shouldReloadMobileGitCommitsAfterAction('push'),
      true,
      'push should reload commits',
    );
    assertEqual(
      shouldReloadMobileGitCommitsAfterAction('merge'),
      false,
      'merge should not reload commits from deleted source worktree',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   删除 active worktree 前必须先让父级 Files dirty guard 决定是否允许离开当前草稿上下文。
   *
   * Code Logic（这个测试做什么）:
   *   构造删除 active 功能 worktree 的计划，断言它需要 active preflight，并预先算出回落到主工作区。
   */
  test('getMobileWorktreeRemovalPlan active removal requires preflight', () => {
    const main = createWorktree('main', true);
    const feature = createWorktree('feature/remove-me', false);

    const plan = getMobileWorktreeRemovalPlan([main, feature], feature.id, feature);

    assertEqual(plan.requiresActivePreflight, true, 'active removal should preflight guard');
    assertEqual(plan.nextActive?.id ?? null, main.id, 'active removal should fall back to main');
    assertEqual(
      plan.nextWorktrees.some((worktree) => worktree.id === feature.id),
      false,
      'removed worktree should be absent from next list',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   删除非 active worktree 不会离开当前 Files 草稿上下文，不能多弹一次 dirty guard。
   *
   * Code Logic（这个测试做什么）:
   *   构造删除非 active 功能 worktree 的计划，断言它不需要 preflight，且 active worktree 保持不变。
   */
  test('getMobileWorktreeRemovalPlan inactive removal skips preflight', () => {
    const main = createWorktree('main', true);
    const feature = createWorktree('feature/remove-me', false);

    const plan = getMobileWorktreeRemovalPlan([main, feature], main.id, feature);

    assertEqual(plan.requiresActivePreflight, false, 'inactive removal should skip preflight');
    assertEqual(plan.nextActive?.id ?? null, main.id, 'inactive removal should keep active');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   删除 active worktree 的确认只是 destructive preflight，不能在后端真正删除前切 active、写列表或丢弃 Files 草稿。
   *
   * Code Logic（这个测试做什么）:
   *   用未完成的 remove promise 卡住后端阶段，断言确认与后端已开始但 apply 回调尚未执行。
   */
  test('runMobileWorktreeRemovalFlow confirm stage does not apply state before backend resolves', async () => {
    const main = createWorktree('main', true);
    const feature = createWorktree('feature/remove-me', false);
    const events: string[] = [];
    let resolveRemove!: () => void;

    const flow = runMobileWorktreeRemovalFlow({
      worktrees: [main, feature],
      activeWorktreeId: feature.id,
      removingWorktree: feature,
      confirmActiveWorktreeChange: () => {
        events.push('confirm');
        return true;
      },
      removeWorktree: async () => {
        events.push('backend');
        await new Promise<void>((resolve) => {
          resolveRemove = resolve;
        });
      },
      applyRemoval: () => {
        events.push('apply');
      },
    });

    assertEqual(
      events.join('>'),
      'confirm>backend',
      'remove preflight should not apply before backend resolves',
    );
    resolveRemove();
    await flow;
    assertEqual(
      events.join('>'),
      'confirm>backend>apply',
      'remove success should apply after backend resolves',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   后端删除 active worktree 失败时，磁盘 worktree 仍存在，移动端不能切走 active 或清空 dirty draft。
   *
   * Code Logic（这个测试做什么）:
   *   模拟 removeWorktree reject，断言流程向外抛错且 applyRemoval 从未执行。
   */
  test('runMobileWorktreeRemovalFlow backend failure does not apply state', async () => {
    const main = createWorktree('main', true);
    const feature = createWorktree('feature/remove-me', false);
    let didApply = false;

    await assertRejects(
      () =>
        runMobileWorktreeRemovalFlow({
          worktrees: [main, feature],
          activeWorktreeId: feature.id,
          removingWorktree: feature,
          confirmActiveWorktreeChange: () => true,
          removeWorktree: async () => {
            throw new Error('remove failed');
          },
          applyRemoval: () => {
            didApply = true;
          },
        }),
      'remove failed',
      'remove backend failure should bubble to component error handling',
    );
    assertEqual(didApply, false, 'remove backend failure should not apply active/list/discard');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   删除 active worktree 成功后，移动端才可以切到回落 worktree、刷新列表并丢弃旧 Files 草稿。
   *
   * Code Logic（这个测试做什么）:
   *   模拟成功删除，断言 applyRemoval 收到已移除源 worktree 的列表和主工作区 nextActive。
   */
  test('runMobileWorktreeRemovalFlow backend success applies next state', async () => {
    const main = createWorktree('main', true);
    const feature = createWorktree('feature/remove-me', false);
    let appliedActiveId: string | null = null;
    let appliedWorktreeIds = '';

    const result = await runMobileWorktreeRemovalFlow({
      worktrees: [main, feature],
      activeWorktreeId: feature.id,
      removingWorktree: feature,
      confirmActiveWorktreeChange: () => true,
      removeWorktree: async () => undefined,
      applyRemoval: (plan) => {
        appliedActiveId = plan.nextActive?.id ?? null;
        appliedWorktreeIds = plan.nextWorktrees.map((worktree) => worktree.id).join(',');
      },
    });

    assertEqual(result, 'applied', 'remove success should report applied transition');
    assertEqual(appliedActiveId, main.id, 'remove success should apply fallback active worktree');
    assertEqual(appliedWorktreeIds, main.id, 'remove success should apply list without removed worktree');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   merge 会删除源 active worktree，必须在调用后端 merge 前先询问 Files dirty guard；用户取消时不能触碰后端。
   *
   * Code Logic（这个测试做什么）:
   *   模拟 confirm 返回 false，断言 mergeWorktree 与 applyMergeSuccess 均不执行。
   */
  /**
   * Business Logic（为什么需要这个测试）:
   *   主工作区 collect-merge 只合入分支、不删除主 worktree；复用删除计划会把当前主工作区从列表滤掉。
   *
   * Code Logic（这个测试做什么）:
   *   以 main 为 source 计算 merge plan，断言保留全部 worktree、保持当前 active，且不要求 dirty preflight。
   */
  test('getMobileWorktreeMergePlan keeps main worktree for collect-merge', () => {
    const main = createWorktree('main', true);
    main.canCollectMerge = true;
    const feature = createWorktree('feature/keep-me', false);

    const plan = getMobileWorktreeMergePlan([main, feature], main.id, main);

    assertEqual(plan.requiresActivePreflight, false, 'collect-merge should skip active preflight');
    assertEqual(plan.nextActive?.id ?? null, main.id, 'collect-merge should keep current active');
    assertEqual(plan.nextWorktrees.length, 2, 'collect-merge should keep the full worktree list');
    assertEqual(
      plan.nextWorktrees.some((worktree) => worktree.id === main.id),
      true,
      'collect-merge must not drop the main worktree',
    );
    assertEqual(
      plan.nextWorktrees.some((worktree) => worktree.id === feature.id),
      true,
      'collect-merge must not drop other worktrees',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   功能 worktree 合回仍会删除源 worktree，collect-merge 计划不能改变这条既有路径。
   *
   * Code Logic（这个测试做什么）:
   *   以 feature 为 source 计算 merge plan，断言仍走删除计划（源从列表消失）。
   */
  test('getMobileWorktreeMergePlan still removes feature worktree', () => {
    const main = createWorktree('main', true);
    const feature = createWorktree('feature/merge-me', false);

    const plan = getMobileWorktreeMergePlan([main, feature], feature.id, feature);

    assertEqual(plan.requiresActivePreflight, true, 'feature merge should preflight when active');
    assertEqual(plan.nextActive?.id ?? null, main.id, 'feature merge should fall back to main');
    assertEqual(
      plan.nextWorktrees.some((worktree) => worktree.id === feature.id),
      false,
      'feature merge should drop the source worktree',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   主工作区 collect-merge 成功后必须保留主 worktree；不能因为 source 是 active 就套用删除计划。
   *
   * Code Logic（这个测试做什么）:
   *   以 main 为 source 跑 merge flow，断言不走 dirty guard，且成功计划仍包含 main。
   */
  test('runMobileWorktreeMergeFlow collect-merge keeps main worktree', async () => {
    const main = createWorktree('main', true);
    main.canCollectMerge = true;
    const feature = createWorktree('feature/keep-me', false);
    let confirmCalls = 0;
    let appliedIds = '';
    let appliedActiveId: string | null = null;
    let appliedRequiresActivePreflight: boolean | null = null;

    const result = await runMobileWorktreeMergeFlow({
      worktrees: [main, feature],
      activeWorktreeId: main.id,
      sourceWorktree: main,
      confirmActiveWorktreeChange: () => {
        confirmCalls += 1;
        return true;
      },
      mergeWorktree: async () => undefined,
      applyMergeSuccess: async (plan) => {
        appliedIds = plan.nextWorktrees.map((worktree) => worktree.id).join(',');
        appliedActiveId = plan.nextActive?.id ?? null;
        appliedRequiresActivePreflight = plan.requiresActivePreflight;
      },
    });

    assertEqual(result, 'applied', 'collect-merge should report applied transition');
    assertEqual(confirmCalls, 0, 'collect-merge should skip dirty guard confirm');
    assertEqual(appliedRequiresActivePreflight, false, 'collect-merge should not require preflight');
    assertEqual(appliedActiveId, main.id, 'collect-merge should keep main active');
    assertEqual(appliedIds, `${main.id},${feature.id}`, 'collect-merge should keep all worktrees');
  });

  test('runMobileWorktreeMergeFlow cancelled by dirty guard does not call backend', async () => {
    const main = createWorktree('main', true);
    const feature = createWorktree('feature/merge-me', false);
    let didCallBackend = false;
    let didApply = false;

    const result = await runMobileWorktreeMergeFlow({
      worktrees: [main, feature],
      activeWorktreeId: feature.id,
      sourceWorktree: feature,
      confirmActiveWorktreeChange: () => false,
      mergeWorktree: async () => {
        didCallBackend = true;
      },
      applyMergeSuccess: async () => {
        didApply = true;
      },
    });

    assertEqual(result, 'cancelled', 'merge cancelled by dirty guard should report cancelled');
    assertEqual(didCallBackend, false, 'merge cancelled by dirty guard should not call backend');
    assertEqual(didApply, false, 'merge cancelled by dirty guard should not apply state');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   合并非 active worktree 不会离开当前 Files 草稿上下文，不能误触发 dirty guard 或切走当前 active。
   *
   * Code Logic（这个测试做什么）:
   *   构造 main 为 active、feature 为 merge source 的流程，断言不调用 confirm，后端 merge 被调用，成功计划保持 main active。
   */
  test('runMobileWorktreeMergeFlow inactive merge skips dirty guard and keeps active', async () => {
    const main = createWorktree('main', true);
    const feature = createWorktree('feature/merge-me', false);
    let confirmCalls = 0;
    let didCallBackend = false;
    let appliedRequiresActivePreflight: boolean | null = null;
    let appliedNextActiveId: string | null = null;

    const result = await runMobileWorktreeMergeFlow({
      worktrees: [main, feature],
      activeWorktreeId: main.id,
      sourceWorktree: feature,
      confirmActiveWorktreeChange: () => {
        confirmCalls += 1;
        return true;
      },
      mergeWorktree: async () => {
        didCallBackend = true;
      },
      applyMergeSuccess: async (plan) => {
        appliedRequiresActivePreflight = plan.requiresActivePreflight;
        appliedNextActiveId = plan.nextActive?.id ?? null;
      },
    });

    assertEqual(result, 'applied', 'inactive merge should report applied transition');
    assertEqual(confirmCalls, 0, 'inactive merge should skip dirty guard confirm');
    assertEqual(didCallBackend, true, 'inactive merge should call backend merge');
    assertEqual(
      appliedRequiresActivePreflight,
      false,
      'inactive merge plan should not require active preflight',
    );
    assertEqual(appliedNextActiveId, main.id, 'inactive merge should keep main active');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   merge 成功会删除源 active worktree，即使后续权威刷新失败，移动端也不能继续指向已删除源 worktree。
   *
   * Code Logic（这个测试做什么）:
   *   构造 merge 删除源 worktree 的计划，断言应用态先移除源列表并切回主工作区。
   */
  test('getMobileWorktreeMergeAppliedState uses removal plan', () => {
    const main = createWorktree('main', true);
    const feature = createWorktree('feature/merge-me', false);
    const state = getMobileWorktreeMergeAppliedState({
      nextWorktrees: [main],
      nextActive: main,
      requiresActivePreflight: true,
    });

    assertEqual(
      state.nextWorktrees.some((worktree) => worktree.id === feature.id),
      false,
      'merge applied state should not keep removed source worktree',
    );
    assertEqual(state.nextActive?.id ?? null, main.id, 'merge applied state should switch to main');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   Git commit/push/merge 等长操作完成时，用户可能已经切到其它项目或 worktree，旧响应不能污染当前 UI。
   *
   * Code Logic（这个测试做什么）:
   *   比较操作发起时和响应时的 project/worktree context，断言任一变化都视为 stale。
   */
  test('isMobileGitActionResponseCurrent requires same context', () => {
    const context = createContext('project-1', 'worktree-1');

    assertEqual(
      isMobileGitActionResponseCurrent(context, context),
      true,
      'same project/worktree action response should be current',
    );
    assertEqual(
      isMobileGitActionResponseCurrent(context, createContext('project-2', 'worktree-1')),
      false,
      'git action response from previous project should be stale',
    );
    assertEqual(
      isMobileGitActionResponseCurrent(context, createContext('project-1', 'worktree-2')),
      false,
      'git action response from previous worktree should be stale',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   active worktree merge 成功返回时，如果用户已经切到其它 worktree，旧响应不能清空当前 worktree 的提交列表。
   *
   * Code Logic（这个测试做什么）:
   *   构造 merge 请求上下文与当前上下文，断言必须保持同项目同 worktree 才视为当前。
   */
  test('isMobileGitMergeResponseCurrent requires source worktree context', () => {
    const sourceContext = createContext('project-1', 'feature');
    const fallbackContext = createContext('project-1', 'main');

    assertEqual(
      isMobileGitMergeResponseCurrent(sourceContext, sourceContext),
      true,
      'merge response should be current for same project/worktree',
    );
    assertEqual(
      isMobileGitMergeResponseCurrent(sourceContext, fallbackContext),
      false,
      'merge response should be stale after same-project different worktree switch',
    );
    assertEqual(
      isMobileGitMergeResponseCurrent(sourceContext, createContext('project-2', 'main')),
      false,
      'merge response from previous project should be stale',
    );
    assertEqual(
      isMobileGitMergeResponseCurrent(sourceContext, null),
      false,
      'merge response should be stale without current context',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   refreshWorktrees 发现 active 已不存在时，需要先确认能否离开 dirty context；取消时不能先写入后端返回的新列表。
   *
   * Code Logic（这个测试做什么）:
   *   传入不含当前 active 的 nextWorktrees，模拟确认取消，断言 applyRefresh 不执行。
   */
  test('runMobileWorktreeRefreshFlow cancel does not apply list or active', () => {
    const main = createWorktree('main', true);
    const feature = createWorktree('feature/deleted-elsewhere', false);
    let didApply = false;

    const result = runMobileWorktreeRefreshFlow({
      nextWorktrees: [main],
      currentActiveWorktreeId: feature.id,
      confirmActiveWorktreeChange: () => false,
      applyRefresh: () => {
        didApply = true;
      },
    });

    assertEqual(result, 'cancelled', 'refresh cancelled by dirty guard should report cancelled');
    assertEqual(didApply, false, 'refresh cancelled by dirty guard should not apply list or active');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   context key 是文件面板 stale guard 的基础，null worktree 必须有稳定表示。
   *
   * Code Logic（这个测试做什么）:
   *   断言同一 project + null worktree 生成稳定 key，null context 返回空 key。
   */
  test('getMobileFileContextKey is stable for null worktree', () => {
    assertEqual(
      getMobileFileContextKey(createContext('project-1', null)),
      'project-1:',
      'null worktree should produce stable key',
    );
    assertEqual(getMobileFileContextKey(null), '', 'null context should produce empty key');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   unknown envelope 必须先 reconciling，对账成功才 confirmed；same-message/different-tree 由 pure matrix 保持 unknown。
   *
   * Code Logic（这个测试做什么）:
   *   断言 resolveMobileMutationPhase 与 operation id 复用/锁定规则。
   */
  test('mobile mutation phase reuses operation id and locks ambiguous unknown', () => {
    assertEqual(
      resolveMobileMutationPhase('succeeded', null),
      'confirmedSucceeded',
      'succeeded envelope should confirm immediately',
    );
    assertEqual(
      resolveMobileMutationPhase('unknown', null),
      'reconciling',
      'unknown without reconcile result should reconciling',
    );
    assertEqual(
      resolveMobileMutationPhase('unknown', 'confirmedSucceeded'),
      'confirmedSucceeded',
      'reconcile confirmed should advance',
    );
    assertEqual(
      resolveMobileMutationPhase('unknown', 'unknown'),
      'unknown',
      'ambiguous reconcile must stay unknown',
    );
    assertEqual(
      resolveMobileMutationPhase('unknown', 'confirmedFailed'),
      'confirmedFailed',
      'ledger failed clears to confirmedFailed',
    );

    assertEqual(
      pickMobileMutationOperationId('unknown', 'op-1', 'op-2'),
      'op-1',
      'unknown must reuse stable operation id',
    );
    assertEqual(
      pickMobileMutationOperationId('idle', null, 'op-2'),
      'op-2',
      'idle may mint new operation id',
    );
    assertEqual(isMobileMutationActionLocked('unknown'), true, 'unknown locks actions');
    assertEqual(isMobileMutationActionLocked('idle'), false, 'idle unlocks actions');
  });
});
