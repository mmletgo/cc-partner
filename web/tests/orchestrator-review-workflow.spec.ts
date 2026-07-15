/**
 * E2E-ORCHESTRATOR-REVIEW-001 — Orchestrator Human Review / WORKFLOW / 通知 / Attention 权威旅程（L1 browser mock）。
 *
 * Business Logic（为什么需要这个套件）:
 *   N6 要求桌面 Human Review 的 Changes→Rework、digest 漂移 Conflict、WORKFLOW 向导
 *   invalid→valid 保存、运营通知 dedupe 与 Attention 深链只导航不执行业务动作。
 *   L1 不宣称真实 Git worktree、OS 通知、多机 owner 或 full-auto delivery 副作用。
 *
 * Code Logic（这个套件做什么）:
 *   backendHarness + appBootstrap 注册 workbench/orchestrator 命令；deep link 打开
 *   automation 控制台与 task drawer；断言 invoke 参数与 privacy/no-action 不变量。
 */

import { expect, test } from './fixtures';
import {
  installAppLocalStorage,
  makeEmptyAttentionSnapshot,
  registerAppShellCommands,
  SETTINGS_FIXTURES,
} from './support/appBootstrap';
import type { PlaywrightBackendHarness } from './support/backendHarness';

const TS = '2026-07-14T00:00:00.000Z';
const PROJECT_ID = 'proj-review';
const TASK_ID = 'task-review-1';
const DIGEST_V1 = 'digest-v1';

/** 禁止从 notification / Attention 路径触发的业务动作命令。 */
const FORBIDDEN_BUSINESS_COMMANDS = [
  'deliver_reviewed_orchestrator_task_view',
  'request_orchestrator_task_rework_view',
  'retry_orchestrator_task_view',
  'cancel_orchestrator_task_view',
  'retry_orchestrator_remote_outbox',
  'discard_orchestrator_remote_outbox',
] as const;

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 深链与侧栏需要合法 project DTO。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖的 local project。
 */
function makeProject(partial: { id: string; name: string; path?: string }) {
  return {
    id: partial.id,
    name: partial.name,
    kind: 'local' as const,
    deviceId: 'device-local',
    deviceName: 'MacBook',
    path: partial.path ?? `/tmp/${partial.id}`,
    lastOpenedAt: TS,
    createdAt: TS,
    updatedAt: TS,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 打开项目后会拉 worktree 列表。
 *
 * Code Logic（这个函数做什么）:
 *   返回主 worktree DTO。
 */
function makeWorktree(projectId: string) {
  return {
    id: `${projectId}:main`,
    projectId,
    name: 'main',
    branch: 'main',
    baseBranch: null,
    path: `/tmp/${projectId}`,
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
    createdAt: TS,
    updatedAt: TS,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 基线 session 列表需要 running session 形态。
 *
 * Code Logic（这个函数做什么）:
 *   返回最小 running session。
 */
function makeSession(projectId: string) {
  return {
    id: `session-${projectId}`,
    projectId,
    worktreeId: null,
    name: 'shell',
    command: '/bin/zsh',
    cwd: `/tmp/${projectId}`,
    status: 'running' as const,
    cols: 80,
    rows: 24,
    startedAt: TS,
    exitedAt: null,
    exitCode: null,
    supportsPanes: true,
    paneCount: 1,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Human Review 旅程要求 status=done / workflowState=humanReview / runState=idle。
 *
 * Code Logic（这个函数做什么）:
 *   返回合法 OrchestratorTask，可用 partial 覆盖字段。
 */
function makeHumanReviewTask(
  partial: {
    id?: string;
    projectId?: string;
    title?: string;
    status?: string;
    workflowState?: string;
    runState?: string;
  } = {},
) {
  return {
    id: partial.id ?? TASK_ID,
    projectId: partial.projectId ?? PROJECT_ID,
    title: partial.title ?? 'Review delivery task',
    goal: 'Ship reviewed changes safely',
    acceptanceCriteria: 'Diff reviewed and delivered or reworked',
    status: partial.status ?? 'done',
    workflowState: partial.workflowState ?? 'humanReview',
    runState: partial.runState ?? 'idle',
    attemptPhase: null,
    source: 'internal',
    externalId: null,
    externalIdentifier: null,
    externalUrl: null,
    externalState: null,
    externalLabels: null,
    runnerProvider: null,
    claudeSessionId: null,
    transcriptPath: null,
    runtimeStartedAt: null,
    lastActivityAt: null,
    lastRuntimeEvent: null,
    lastRuntimeMessage: null,
    priority: 0,
    branchName: 'agent/task-review',
    worktreeId: `${partial.projectId ?? PROJECT_ID}:main`,
    sessionId: null,
    blockedReason: null,
    attempt: 1,
    createdAt: TS,
    updatedAt: TS,
    startedAt: TS,
    finishedAt: TS,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   看板状态条与 workflow 向导入口依赖 runtime snapshot。
 *
 * Code Logic（这个函数做什么）:
 *   返回最小合法 runtime snapshot。
 */
function makeRuntime(projectId: string) {
  return {
    projectId,
    projectKind: 'local' as const,
    remoteStatus: 'local' as const,
    generatedAt: TS,
    latestTickAt: null,
    lastDispatchAt: null,
    lastDispatchedCount: 0,
    schedulerEnabled: true,
    workflowSource: 'builtin',
    workflowValid: true,
    workflowError: null,
    maxConcurrentTasks: 1,
    slotsUsed: 0,
    slotsAvailable: 1,
    latestError: null,
    runningTasks: [],
    retryingTasks: [],
    recentEvents: [],
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Changes tab 与 Deliver digest 门闩需要合法 OrchestratorReviewDiff。
 *
 * Code Logic（这个函数做什么）:
 *   返回 decoder 直连 diff 对象（非 {diff:...} 包装），可覆盖 digest/path。
 */
function makeReviewDiff(
  partial: {
    taskId?: string;
    reviewDigest?: string;
    path?: string;
    patch?: string;
  } = {},
) {
  const path = partial.path ?? 'src/feature.ts';
  const patch =
    partial.patch ?? '@@ -1,2 +1,3 @@\n-old\n+new line\n context\n';
  return {
    taskId: partial.taskId ?? TASK_ID,
    baseRef: 'main',
    headRef: 'agent/task-review',
    files: [
      {
        path,
        status: 'modified',
        additions: 1,
        deletions: 1,
        patch,
        binary: false,
        truncated: false,
      },
    ],
    totalFiles: 1,
    truncated: false,
    reviewDigest: partial.reviewDigest ?? DIGEST_V1,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   运营通知偏好与 Settings automation 需要完整 notify* 布尔字段。
 *
 * Code Logic（这个函数做什么）:
 *   基于 SETTINGS_FIXTURES.automation 覆盖 notify 开关。
 */
function makeOrchestratorConfig(
  partial: {
    notifyHumanReview?: boolean;
    notifyBlocked?: boolean;
    notifyRemoteOutboxFailed?: boolean;
    notifyTaskDone?: boolean;
  } = {},
) {
  return {
    ...SETTINGS_FIXTURES.automation,
    notifyHumanReview: partial.notifyHumanReview ?? true,
    notifyBlocked: partial.notifyBlocked ?? true,
    notifyRemoteOutboxFailed: partial.notifyRemoteOutboxFailed ?? true,
    notifyTaskDone: partial.notifyTaskDone ?? false,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Attention 列表需要合法 Inbox item 深链到 automation task。
 *
 * Code Logic（这个函数做什么）:
 *   返回 orchestratorHumanReview 条目。
 */
function makeAttentionItem(partial: {
  id?: string;
  projectId: string;
  taskId: string;
  title?: string;
}) {
  return {
    id: partial.id ?? `orchestrator:human-review:${partial.taskId}`,
    category: 'decision' as const,
    sourceKind: 'orchestratorHumanReview' as const,
    title: partial.title ?? 'Review delivery',
    summary: 'Need human review',
    updatedAt: TS,
    freshness: 'live' as const,
    cachedAt: null,
    project: { id: partial.projectId, name: 'Review Project', kind: 'local' as const },
    device: null,
    target: {
      kind: 'orchestratorTask' as const,
      projectId: partial.projectId,
      taskId: partial.taskId,
    },
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 页面会并发拉 worktrees/sessions/files/git/focus。
 *
 * Code Logic（这个函数做什么）:
 *   在 AppShell 基线上注册 sticky 空/就绪默认。
 */
function registerWorkbenchBaseline(harness: PlaywrightBackendHarness): void {
  registerAppShellCommands(harness);
  harness.command('list_workbench_worktrees', { kind: 'resolve', value: [] });
  harness.command('list_workbench_sessions', { kind: 'resolve', value: [] });
  harness.command('list_workbench_dir', { kind: 'resolve', value: [] });
  harness.command('list_workbench_git_commits', { kind: 'resolve', value: [] });
  harness.command('get_focused_workbench_session', {
    kind: 'resolve',
    value: { sessionId: null },
  });
  harness.command('focus_workbench_session', {
    kind: 'resolve',
    value: { ok: true, sessionId: 'session-placeholder' },
  });
  harness.command('get_workbench_path_info', {
    kind: 'resolve',
    value: {
      name: 'README.md',
      path: 'README.md',
      kind: 'file',
      size: 12,
      modifiedAt: TS,
    },
  });
  harness.command('touch_workbench_project', {
    kind: 'resolve',
    value: makeProject({ id: 'touch', name: 'touch' }),
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Orchestrator 看板挂载会拉 task views / runtime / evidence，通知协调器会拉 snapshot/config。
 *
 * Code Logic（这个函数做什么）:
 *   注册 sticky orchestrator 读路径与运营通知 baseline。
 */
function registerOrchestratorReadBaseline(
  harness: PlaywrightBackendHarness,
  options: {
    projectId: string;
    task?: ReturnType<typeof makeHumanReviewTask>;
    reviewDiff?: ReturnType<typeof makeReviewDiff>;
    attentionItems?: ReturnType<typeof makeAttentionItem>[];
  },
): void {
  const task = options.task ?? makeHumanReviewTask({ projectId: options.projectId });
  const reviewDiff = options.reviewDiff ?? makeReviewDiff({ taskId: task.id });
  const attentionItems = options.attentionItems ?? [];

  harness.command('list_orchestrator_task_views', {
    kind: 'resolve',
    value: [{ origin: 'local', task }],
  });
  harness.command('get_orchestrator_review_diff', {
    kind: 'resolve',
    value: reviewDiff,
  });
  harness.command('list_orchestrator_task_evidence_for_project', {
    kind: 'resolve',
    value: [],
  });
  harness.command('get_orchestrator_runtime_snapshot', {
    kind: 'resolve',
    value: makeRuntime(options.projectId),
  });
  harness.command('get_operational_notification_snapshot', {
    kind: 'resolve',
    value: {
      asOfCursor: { ownerInstanceId: 'owner-1', sequence: 0 },
      items: [],
      truncated: false,
    },
  });
  harness.command('get_orchestrator_config', {
    kind: 'resolve',
    value: makeOrchestratorConfig(),
  });
  harness.command('get_default_orchestrator_config', {
    kind: 'resolve',
    value: makeOrchestratorConfig(),
  });
  harness.command('list_attention_items', {
    kind: 'resolve',
    value: {
      ...makeEmptyAttentionSnapshot(),
      counts: {
        total: attentionItems.length,
        decision: attentionItems.filter((item) => item.category === 'decision').length,
        blocked: attentionItems.filter((item) => item.category === 'blocked').length,
        environment: attentionItems.filter((item) => item.category === 'environment').length,
      },
      items: attentionItems,
    },
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   通知/Attention 路径不得触发 Deliver/Rework/Retry/Discard 等业务动作。
 *
 * Code Logic（这个函数做什么）:
 *   统计 harness.calls 中指定命令的 invoke 次数。
 */
function countInvokes(
  harness: PlaywrightBackendHarness,
  command: string,
): number {
  return harness
    .calls()
    .filter((call) => call.type === 'invoke' && call.command === command).length;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   跨旅程断言 privacy/no-action 不变量。
 *
 * Code Logic（这个函数做什么）:
 *   对 FORBIDDEN_BUSINESS_COMMANDS 断言 invoke 次数为 0。
 */
function assertNoBusinessActionInvokes(harness: PlaywrightBackendHarness): void {
  for (const command of FORBIDDEN_BUSINESS_COMMANDS) {
    expect(
      countInvokes(harness, command),
      `expected zero invokes for ${command}`,
    ).toBe(0);
  }
  expect(
    countInvokes(harness, 'plugin:notification|register_action_types'),
    'notification path must not register action types',
  ).toBe(0);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   深链打开 automation 后需要等待看板/任务详情就绪。
 *
 * Code Logic（这个函数做什么）:
 *   等待任务卡 aria（queue.taskAria）或已打开的任务详情 drawer 标题。
 */
async function expectAutomationReady(
  page: import('@playwright/test').Page,
  taskTitle: string,
): Promise<void> {
  const taskCard = page.getByRole('button', {
    name: new RegExp(`选择任务\\s*${taskTitle}|${taskTitle}`),
  });
  const drawer = page.getByRole('dialog').filter({ hasText: taskTitle });
  await expect(taskCard.or(drawer)).toBeVisible({ timeout: 20_000 });
}

test.describe('E2E-ORCHESTRATOR-REVIEW-001 Orchestrator review workflow', () => {
  test('human review diff → request rework with reason', async ({
    page,
    backendHarness,
  }) => {
    const project = makeProject({ id: PROJECT_ID, name: 'Review Project' });
    const task = makeHumanReviewTask({
      projectId: PROJECT_ID,
      title: 'Patch boundary review',
    });
    const reworkedTask = makeHumanReviewTask({
      projectId: PROJECT_ID,
      title: 'Patch boundary review',
      status: 'queued',
      workflowState: 'rework',
      runState: 'idle',
    });
    const reviewDiff = makeReviewDiff({
      taskId: task.id,
      path: 'src/boundary.ts',
      patch: '@@ -1 +1 @@\n-unsafe\n+safe\n',
      reviewDigest: DIGEST_V1,
    });

    await installAppLocalStorage(page);
    registerWorkbenchBaseline(backendHarness);
    registerOrchestratorReadBaseline(backendHarness, {
      projectId: PROJECT_ID,
      task,
      reviewDiff,
    });

    backendHarness.command('list_workbench_projects', {
      kind: 'resolve',
      value: [project],
    });
    backendHarness.command('list_workbench_worktrees', {
      kind: 'resolve',
      value: [makeWorktree(PROJECT_ID)],
    });
    backendHarness.command('list_workbench_sessions', {
      kind: 'resolve',
      value: [makeSession(PROJECT_ID)],
    });
    backendHarness.command('touch_workbench_project', {
      kind: 'resolve',
      value: project,
    });
    backendHarness.command('request_orchestrator_task_rework_view', {
      kind: 'resolve',
      value: { origin: 'local', task: reworkedTask },
    });

    await page.addInitScript((projectId) => {
      window.localStorage.setItem('cp-workbench-active-project-id', projectId);
    }, PROJECT_ID);

    await page.goto(
      `/workbench?projectId=${PROJECT_ID}&view=automation&taskId=${TASK_ID}`,
    );
    await expectAutomationReady(page, task.title);

    // deep link 聚焦任务抽屉（accessible name 为任务标题）
    await expect(page.getByRole('dialog').filter({ hasText: task.title })).toBeVisible({
      timeout: 15_000,
    });

    // Changes tab：文件路径与 patch 可见
    await page.getByRole('tab', { name: '变更' }).click();
    await expect(page.getByText('src/boundary.ts')).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText('unsafe').or(page.getByText('+safe'))).toBeVisible({
      timeout: 10_000,
    });

    // Request Rework → 填原因 → 提交
    await page.getByRole('button', { name: '要求返工' }).click();
    const reason = page.locator('#orchestrator-rework-reason');
    await expect(reason).toBeVisible({ timeout: 5_000 });
    await reason.fill('边界校验仍有风险，请按反馈返工');
    await page.getByRole('button', { name: '提交返工' }).click();

    await expect
      .poll(() => countInvokes(backendHarness, 'request_orchestrator_task_rework_view'), {
        timeout: 10_000,
      })
      .toBe(1);

    const reworkCall = backendHarness
      .calls()
      .find(
        (call) =>
          call.type === 'invoke' &&
          call.command === 'request_orchestrator_task_rework_view',
      );
    expect(reworkCall?.type).toBe('invoke');
    if (reworkCall?.type === 'invoke') {
      const args = reworkCall.args as {
        projectId?: string;
        taskId?: string;
        reason?: string;
      };
      expect(args.projectId).toBe(PROJECT_ID);
      expect(args.taskId).toBe(TASK_ID);
      expect(args.reason).toBe('边界校验仍有风险，请按反馈返工');
    }
  });

  test('hidden-tail digest drift → deliver conflict and re-fetch', async ({
    page,
    backendHarness,
  }) => {
    const project = makeProject({ id: PROJECT_ID, name: 'Review Project' });
    const task = makeHumanReviewTask({
      projectId: PROJECT_ID,
      title: 'Digest drift task',
    });
    const reviewDiff = makeReviewDiff({
      taskId: task.id,
      reviewDigest: DIGEST_V1,
      path: 'src/drift.ts',
    });

    await installAppLocalStorage(page);
    registerWorkbenchBaseline(backendHarness);
    registerOrchestratorReadBaseline(backendHarness, {
      projectId: PROJECT_ID,
      task,
      reviewDiff,
    });

    backendHarness.command('list_workbench_projects', {
      kind: 'resolve',
      value: [project],
    });
    backendHarness.command('list_workbench_worktrees', {
      kind: 'resolve',
      value: [makeWorktree(PROJECT_ID)],
    });
    backendHarness.command('list_workbench_sessions', {
      kind: 'resolve',
      value: [makeSession(PROJECT_ID)],
    });
    backendHarness.command('touch_workbench_project', {
      kind: 'resolve',
      value: project,
    });
    // 初次 getReviewDiff 用 digest-v1；冲突后重拉仍 sticky 返回 v1（足以断言 re-fetch 与 expectedReviewDigest）
    backendHarness.command('get_orchestrator_review_diff', {
      kind: 'resolve',
      value: reviewDiff,
    });
    backendHarness.command('deliver_reviewed_orchestrator_task_view', {
      kind: 'reject',
      error: new Error('review_diff_changed: hidden-tail digest drifted'),
    });

    await page.addInitScript((projectId) => {
      window.localStorage.setItem('cp-workbench-active-project-id', projectId);
    }, PROJECT_ID);

    await page.goto(
      `/workbench?projectId=${PROJECT_ID}&view=automation&taskId=${TASK_ID}`,
    );
    await expectAutomationReady(page, task.title);

    await page.getByRole('tab', { name: '变更' }).click();
    await expect(page.getByText('src/drift.ts')).toBeVisible({ timeout: 10_000 });

    const deliver = page.getByRole('button', { name: '交付' });
    await expect(deliver).toBeEnabled({ timeout: 10_000 });
    await deliver.click();

    await expect
      .poll(() => countInvokes(backendHarness, 'deliver_reviewed_orchestrator_task_view'), {
        timeout: 10_000,
      })
      .toBe(1);

    const deliverCall = backendHarness
      .calls()
      .find(
        (call) =>
          call.type === 'invoke' &&
          call.command === 'deliver_reviewed_orchestrator_task_view',
      );
    expect(deliverCall?.type).toBe('invoke');
    if (deliverCall?.type === 'invoke') {
      const args = deliverCall.args as {
        projectId?: string;
        taskId?: string;
        expectedReviewDigest?: string;
      };
      expect(args.projectId).toBe(PROJECT_ID);
      expect(args.taskId).toBe(TASK_ID);
      expect(args.expectedReviewDigest).toBe(DIGEST_V1);
    }

    await expect(
      page.getByText('审阅后的变更已漂移，请重新审阅后再交付。'),
    ).toBeVisible({ timeout: 10_000 });

    // 冲突后至少再拉一次 review diff（初始加载 + 冲突重拉）
    await expect
      .poll(() => countInvokes(backendHarness, 'get_orchestrator_review_diff'), {
        timeout: 10_000,
      })
      .toBeGreaterThanOrEqual(2);
  });

  test('WORKFLOW invalid → validate → save without delivery side effects', async ({
    page,
    backendHarness,
  }) => {
    const project = makeProject({ id: PROJECT_ID, name: 'Review Project' });
    const task = makeHumanReviewTask({
      projectId: PROJECT_ID,
      title: 'Workflow wizard task',
    });
    const validContent =
      '---\nworkflow:\n  default_create_state: backlog\n---\n# steps\n';

    await installAppLocalStorage(page);
    registerWorkbenchBaseline(backendHarness);
    registerOrchestratorReadBaseline(backendHarness, {
      projectId: PROJECT_ID,
      task,
    });

    backendHarness.command('list_workbench_projects', {
      kind: 'resolve',
      value: [project],
    });
    backendHarness.command('list_workbench_worktrees', {
      kind: 'resolve',
      value: [makeWorktree(PROJECT_ID)],
    });
    backendHarness.command('list_workbench_sessions', {
      kind: 'resolve',
      value: [makeSession(PROJECT_ID)],
    });
    backendHarness.command('touch_workbench_project', {
      kind: 'resolve',
      value: project,
    });
    backendHarness.command('get_workflow_document', {
      kind: 'resolve',
      value: {
        status: 'invalid',
        content: 'bad',
        contentHash: 'hash1',
        diagnostics: [
          {
            path: 'WORKFLOW.md',
            line: 1,
            column: 1,
            code: 'parse',
            message: 'bad yaml',
          },
        ],
        preview: null,
      },
    });
    backendHarness.command('validate_workflow_document', {
      kind: 'resolve',
      value: {
        status: 'valid',
        content: validContent,
        contentHash: 'hash1',
        diagnostics: [],
        preview: null,
      },
    });
    backendHarness.command('save_workflow_document', {
      kind: 'resolve',
      value: {
        status: 'valid',
        content: validContent,
        contentHash: 'hash2',
        diagnostics: [],
        preview: null,
      },
    });

    await page.addInitScript((projectId) => {
      window.localStorage.setItem('cp-workbench-active-project-id', projectId);
    }, PROJECT_ID);

    // 不带 taskId，避免任务抽屉 backdrop 挡住 WORKFLOW 向导按钮
    await page.goto(`/workbench?projectId=${PROJECT_ID}&view=automation`);
    await expect(page.getByTestId('open-workflow-wizard')).toBeVisible({
      timeout: 20_000,
    });

    await page.getByTestId('open-workflow-wizard').click();
    await expect(page.getByTestId('workflow-wizard-diagnostics')).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByTestId('workflow-wizard-diagnostic-0')).toContainText(
      'bad yaml',
    );

    const draft = page.getByTestId('workflow-wizard-draft');
    await draft.fill(validContent);
    await page.getByTestId('workflow-wizard-validate').click();
    await expect
      .poll(() => countInvokes(backendHarness, 'validate_workflow_document'), {
        timeout: 10_000,
      })
      .toBe(1);
    await expect(page.getByTestId('workflow-wizard-summary')).toBeVisible({
      timeout: 10_000,
    });

    const deliverBefore = countInvokes(
      backendHarness,
      'deliver_reviewed_orchestrator_task_view',
    );
    const startBefore = countInvokes(backendHarness, 'start_orchestrator_task_view');

    await page.getByTestId('workflow-wizard-save').click();
    await expect
      .poll(() => countInvokes(backendHarness, 'save_workflow_document'), {
        timeout: 10_000,
      })
      .toBe(1);

    const saveCall = backendHarness
      .calls()
      .find(
        (call) =>
          call.type === 'invoke' && call.command === 'save_workflow_document',
      );
    expect(saveCall?.type).toBe('invoke');
    if (saveCall?.type === 'invoke') {
      const args = saveCall.args as {
        projectId?: string;
        expectedHash?: string;
        content?: string;
      };
      expect(args.projectId).toBe(PROJECT_ID);
      expect(args.expectedHash).toBe('hash1');
      expect(args.content).toContain('default_create_state');
    }

    expect(countInvokes(backendHarness, 'deliver_reviewed_orchestrator_task_view')).toBe(
      deliverBefore,
    );
    expect(countInvokes(backendHarness, 'start_orchestrator_task_view')).toBe(
      startBefore,
    );
  });

  test('informational notification send/dedupe does not execute business actions', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerAppShellCommands(backendHarness);
    backendHarness.command('get_operational_notification_snapshot', {
      kind: 'resolve',
      value: {
        asOfCursor: { ownerInstanceId: 'owner-1', sequence: 0 },
        items: [],
        truncated: false,
      },
    });
    backendHarness.command('get_orchestrator_config', {
      kind: 'resolve',
      value: makeOrchestratorConfig({
        notifyHumanReview: true,
        notifyBlocked: true,
        notifyRemoteOutboxFailed: true,
        notifyTaskDone: false,
      }),
    });
    backendHarness.command('list_attention_items', {
      kind: 'resolve',
      value: makeEmptyAttentionSnapshot(),
    });

    // 停留在 Home，避免 /workbench|/attention 前台抑制 OS notify 路径
    await page.goto('/');
    await expect
      .poll(
        () => countInvokes(backendHarness, 'get_operational_notification_snapshot'),
        { timeout: 15_000 },
      )
      .toBeGreaterThan(0);

    const baselineBusiness = FORBIDDEN_BUSINESS_COMMANDS.map((command) =>
      countInvokes(backendHarness, command),
    );
    const baselineRegister = countInvokes(
      backendHarness,
      'plugin:notification|register_action_types',
    );

    const event = {
      kind: 'humanReview',
      opaqueSourceId: 'src-1',
      stateVersion: 1,
      occurredAt: '2026-07-14T01:00:00.000Z',
      ownerInstanceId: 'owner-1',
      sequence: 1,
    };
    backendHarness.emit('operational:notification', event);
    // 同 key 再发一次 → dedupe，不得新增业务动作
    backendHarness.emit('operational:notification', event);

    // 给 coordinator 处理事件与 Attention invalidation 一点时间
    await page.waitForTimeout(500);

    for (let index = 0; index < FORBIDDEN_BUSINESS_COMMANDS.length; index += 1) {
      const command = FORBIDDEN_BUSINESS_COMMANDS[index]!;
      expect(
        countInvokes(backendHarness, command),
        `notification path must not invoke ${command}`,
      ).toBe(baselineBusiness[index]);
    }
    expect(countInvokes(backendHarness, 'plugin:notification|register_action_types')).toBe(
      baselineRegister,
    );
  });

  test('Attention deep link navigates to automation authority without business actions', async ({
    page,
    backendHarness,
  }) => {
    const project = makeProject({ id: PROJECT_ID, name: 'Review Project' });
    const task = makeHumanReviewTask({
      projectId: PROJECT_ID,
      title: 'Attention authority task',
    });
    const attentionItem = makeAttentionItem({
      projectId: PROJECT_ID,
      taskId: TASK_ID,
      title: 'Attention authority task',
    });

    await installAppLocalStorage(page);
    registerWorkbenchBaseline(backendHarness);
    registerOrchestratorReadBaseline(backendHarness, {
      projectId: PROJECT_ID,
      task,
      attentionItems: [attentionItem],
    });
    backendHarness.command('list_workbench_projects', {
      kind: 'resolve',
      value: [project],
    });
    backendHarness.command('list_workbench_worktrees', {
      kind: 'resolve',
      value: [makeWorktree(PROJECT_ID)],
    });
    backendHarness.command('list_workbench_sessions', {
      kind: 'resolve',
      value: [makeSession(PROJECT_ID)],
    });
    backendHarness.command('touch_workbench_project', {
      kind: 'resolve',
      value: project,
    });

    await page.addInitScript((projectId) => {
      window.localStorage.setItem('cp-workbench-active-project-id', projectId);
    }, PROJECT_ID);

    await page.goto('/attention');
    await expect(page.getByTestId(`attention-item-${attentionItem.id}`)).toBeVisible({
      timeout: 15_000,
    });

    const before = FORBIDDEN_BUSINESS_COMMANDS.map((command) =>
      countInvokes(backendHarness, command),
    );

    await page.getByTestId(`attention-item-${attentionItem.id}`).click();

    await expect(page).toHaveURL(
      new RegExp(
        `/workbench\\?projectId=${PROJECT_ID}&view=automation&taskId=${TASK_ID}`,
      ),
      { timeout: 15_000 },
    );

    // 点击本身不得触发业务动作；随后看板加载 list 命令是允许的
    await page.waitForTimeout(400);
    for (let index = 0; index < FORBIDDEN_BUSINESS_COMMANDS.length; index += 1) {
      const command = FORBIDDEN_BUSINESS_COMMANDS[index]!;
      expect(
        countInvokes(backendHarness, command),
        `attention click must not invoke ${command}`,
      ).toBe(before[index]);
    }
  });

  test('notification and attention paths share no-action invariant', async ({
    page,
    backendHarness,
  }) => {
    const project = makeProject({ id: PROJECT_ID, name: 'Review Project' });
    const task = makeHumanReviewTask({
      projectId: PROJECT_ID,
      title: 'No-action invariant task',
    });
    const attentionItem = makeAttentionItem({
      projectId: PROJECT_ID,
      taskId: TASK_ID,
      title: 'No-action invariant task',
    });

    await installAppLocalStorage(page);
    registerWorkbenchBaseline(backendHarness);
    registerOrchestratorReadBaseline(backendHarness, {
      projectId: PROJECT_ID,
      task,
      attentionItems: [attentionItem],
    });
    backendHarness.command('list_workbench_projects', {
      kind: 'resolve',
      value: [project],
    });
    backendHarness.command('list_workbench_worktrees', {
      kind: 'resolve',
      value: [makeWorktree(PROJECT_ID)],
    });
    backendHarness.command('list_workbench_sessions', {
      kind: 'resolve',
      value: [makeSession(PROJECT_ID)],
    });
    backendHarness.command('touch_workbench_project', {
      kind: 'resolve',
      value: project,
    });

    await page.addInitScript((projectId) => {
      window.localStorage.setItem('cp-workbench-active-project-id', projectId);
    }, PROJECT_ID);

    // 先在 Home 触发 notification（不抑制）
    await page.goto('/');
    await expect
      .poll(
        () => countInvokes(backendHarness, 'get_operational_notification_snapshot'),
        { timeout: 15_000 },
      )
      .toBeGreaterThan(0);

    backendHarness.emit('operational:notification', {
      kind: 'humanReview',
      opaqueSourceId: 'src-invariant',
      stateVersion: 1,
      occurredAt: '2026-07-14T02:00:00.000Z',
      ownerInstanceId: 'owner-1',
      sequence: 2,
    });
    await page.waitForTimeout(300);
    assertNoBusinessActionInvokes(backendHarness);

    // 再点 Attention 行导航
    await page.goto('/attention');
    await expect(page.getByTestId(`attention-item-${attentionItem.id}`)).toBeVisible({
      timeout: 15_000,
    });
    await page.getByTestId(`attention-item-${attentionItem.id}`).click();
    await expect(page).toHaveURL(new RegExp(`view=automation&taskId=${TASK_ID}`), {
      timeout: 15_000,
    });
    await page.waitForTimeout(400);
    assertNoBusinessActionInvokes(backendHarness);
  });
});
