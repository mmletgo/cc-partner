import { describe, test } from 'vitest';
import { readFileSync } from 'node:fs';

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端自动化面板必须与桌面端保持同一创建任务体验，回归测试需要在表单退回固定内嵌时失败。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛出 Error，让测试用例失败。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   静态契约测试需要确认关键源码片段存在，避免移动端创建任务缺少 dialog 或 AI 完善入口。
 *
 * Code Logic（这个函数做什么）:
 *   检查源码包含指定字符串；缺失时抛出带上下文的错误。
 */
function assertContains(source: string, expected: string, message: string): void {
  assert(source.includes(expected), message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端创建表单不应固定在页面主文档流中，测试需要锁定旧结构被移除。
 *
 * Code Logic（这个函数做什么）:
 *   检查源码不包含指定字符串；存在时抛出带上下文的错误。
 */
function assertNotContains(source: string, unexpected: string, message: string): void {
  assert(!source.includes(unexpected), message);
}

describe('MobileAutomationPanel', () => {
  test('mobile automation panel keeps dialog/transport/locale/static contracts', () => {
    const panelSource = readFileSync(
      new URL('./components/MobileAutomationPanel.tsx', import.meta.url),
      'utf8',
    );
    const controllerSource = readFileSync(
      new URL('./controllers/useMobileAutomationController.ts', import.meta.url),
      'utf8',
    );
    const createDialogSource = readFileSync(
      new URL('./components/MobileAutomationCreateDialog.tsx', import.meta.url),
      'utf8',
    );
    const taskListSource = readFileSync(
      new URL('./components/MobileAutomationTaskList.tsx', import.meta.url),
      'utf8',
    );
    const taskDetailSource = readFileSync(
      new URL('./components/MobileAutomationTaskDetail.tsx', import.meta.url),
      'utf8',
    );
    const outboxSource = readFileSync(
      new URL('./components/MobileAutomationOutbox.tsx', import.meta.url),
      'utf8',
    );
    const mobileWorkbenchSource = readFileSync(new URL('./MobileWorkbench.tsx', import.meta.url), 'utf8');
    const stylesSource = readFileSync(new URL('./MobileWorkbench.module.css', import.meta.url), 'utf8');
    const workbenchHttpSource = readFileSync(new URL('../api/workbenchHttp.ts', import.meta.url), 'utf8');
    const zhWorkbench = readFileSync(new URL('../i18n/locales/zh/workbench.json', import.meta.url), 'utf8');
    const enWorkbench = readFileSync(new URL('../i18n/locales/en/workbench.json', import.meta.url), 'utf8');
    const typesSource = readFileSync(new URL('../lib/types/orchestrator.ts', import.meta.url), 'utf8');

    // Ownership: views/panel must not own transport/API; controller must own transport and not Dialog JSX.
    for (const [name, source] of [
      ['panel', panelSource],
      ['taskList', taskListSource],
      ['taskDetail', taskDetailSource],
      ['createDialog', createDialogSource],
      ['outbox', outboxSource],
    ] as const) {
      assertNotContains(
        source,
        'httpOrchestratorTransport',
        `${name} must not import or call httpOrchestratorTransport`,
      );
      assertNotContains(
        source,
        '@/api/',
        `${name} must not import @/api/* modules`,
      );
    }
    assertContains(
      controllerSource,
      'httpOrchestratorTransport',
      'controller should own orchestrator transport calls',
    );
    assertNotContains(
      controllerSource,
      'getReviewDiff',
      'A0 removes mobile review diff loading from controller',
    );
    assertNotContains(
      taskDetailSource,
      'reviewDiff',
      'A0 removes review diff product surface from mobile detail',
    );

            assertNotContains(
      taskDetailSource,
      'deliver',
      'mobile detail must not expose deliver action this track',
    );
    assertNotContains(
      taskDetailSource,
      'requestRework',
      'mobile detail must not expose rework action this track',
    );
        assertContains(
      taskDetailSource,
      'role="alert"',
      'mobile review/evidence errors must use role=alert',
    );

    assertNotContains(
      controllerSource,
      '<Dialog',
      'controller must not render Dialog JSX tree',
    );
    assertNotContains(
      controllerSource,
      'pendingRemoteItems.map',
      'controller must not render outbox item map trees',
    );
    assertNotContains(
      controllerSource,
      'groupedTasks[workflowState].map',
      'controller must not render task-row map trees',
    );
    assertContains(
      createDialogSource,
      '<Dialog',
      'create dialog component should own Dialog JSX',
    );

    assertContains(
      controllerSource,
      'const [createDialogOpen, setCreateDialogOpen] = useState<boolean>(false);',
      'mobile automation task creation should use dialog open state',
    );
    assertContains(createDialogSource, '<Dialog', 'mobile automation task creation should render shared Dialog');
    assertContains(
      createDialogSource,
      "from '@/components/primitives'",
      'mobile automation should import Dialog from primitives',
    );
    assertContains(createDialogSource, 'titleId={dialogTitleId}', 'mobile automation Dialog should wire titleId');
    assertContains(
      createDialogSource,
      'closeOnEscape={!(creating || completingPrompt)}',
      'mobile automation Dialog should block Escape while creating/completing',
    );
    assertContains(
      createDialogSource,
      'closeOnBackdrop={!(creating || completingPrompt)}',
      'mobile automation Dialog should block backdrop close while creating/completing',
    );
    assertContains(
      createDialogSource,
      'initialFocusRef={promptDraftRef}',
      'mobile automation Dialog should focus short prompt on open',
    );
    assertNotContains(
      createDialogSource,
      'role="dialog"',
      'mobile automation should not hand-write dialog role',
    );
    assertNotContains(
      createDialogSource,
      'aria-modal="true"',
      'mobile automation should not hand-write aria-modal',
    );
    assertNotContains(
      createDialogSource,
      "window.addEventListener('keydown'",
      'mobile automation Escape should be owned by Dialog, not local listener',
    );
    assertNotContains(
      panelSource,
      'role="dialog"',
      'panel should not hand-write dialog role',
    );
    assertContains(
      controllerSource,
      'httpOrchestratorTransport.tasks.completePrompt',
      'mobile automation task creation should call AI prompt completion through HTTP transport',
    );
    assertContains(
      controllerSource,
      'httpOrchestratorTransport.tasks.listViews',
      'mobile automation should list local or remote tasks through task view HTTP proxy',
    );
    assertContains(
      controllerSource,
      'httpOrchestratorTransport.outbox.retry',
      'mobile automation should retry failed outbox through local HTTP route',
    );
    assertContains(
      controllerSource,
      'httpOrchestratorTransport.outbox.discard',
      'mobile automation should discard failed outbox through local HTTP route',
    );
    assertContains(
      outboxSource,
      "item.status === 'failed'",
      'mobile outbox actions should render only for failed status',
    );
    assertContains(
      controllerSource,
      "window.confirm(t('workbench:mobile.automationPanel.pendingDiscardConfirm'))",
      'mobile discard should require confirmation',
    );
    assertContains(
      zhWorkbench,
      '"pendingRetry"',
      'zh workbench mobile automation should include pendingRetry',
    );
    assertContains(
      enWorkbench,
      '"pendingDiscard"',
      'en workbench mobile automation should include pendingDiscard',
    );

    assertContains(
      controllerSource,
      'httpOrchestratorTransport.getRuntimeSnapshot',
      'mobile automation should load remote-aware runtime snapshot through mobile HTTP route',
    );
    assertContains(
      controllerSource,
      'applyMobileRuntimeSnapshotSuccess',
      'mobile automation should apply in-memory runtime snapshot cache store',
    );
    assertContains(
      controllerSource,
      'toRuntimeLoadError',
      'mobile automation runtime load catch must preserve transport Error kind via toRuntimeLoadError',
    );
    assertNotContains(
      controllerSource,
      'new Error(getErrorMessage(reason))',
      'mobile automation must not rewrap runtime load errors and drop transport kind',
    );
    assertContains(
      panelSource,
      'runtimeDisplay.snapshot.generatedAt',
      'mobile runtime strip must render owner generatedAt',
    );
    assertContains(
      panelSource,
      'runtimeDisplay.snapshot.latestTickAt',
      'mobile runtime strip must render owner latestTickAt',
    );
    assertContains(
      panelSource,
      'runtimeDisplay.snapshot.recentEvents',
      'mobile runtime strip must render owner recentEvents',
    );
    assertContains(
      panelSource,
      'runtimeGeneratedAt',
      'mobile automation must reference runtimeGeneratedAt i18n key',
    );
    assertContains(
      panelSource,
      'runtimeLatestTickAt',
      'mobile automation must reference runtimeLatestTickAt i18n key',
    );
    assertContains(
      panelSource,
      'runtimeRecentEvents',
      'mobile automation must reference runtimeRecentEvents i18n key',
    );
    assertContains(
      zhWorkbench,
      '"runtimeGeneratedAt": "生成时间 {{time}}"',
      'zh locale must include runtime generatedAt copy',
    );
    assertContains(
      enWorkbench,
      '"runtimeGeneratedAt": "Generated {{time}}"',
      'en locale must include runtime generatedAt copy',
    );
    assertContains(
      zhWorkbench,
      '"runtimeRecentEvents": "最近事件"',
      'zh locale must include recent events copy',
    );
    assertContains(
      enWorkbench,
      '"runtimeRecentEvents": "Recent events"',
      'en locale must include recent events copy',
    );
    assertContains(
      panelSource,
      'runtimeCachedHint',
      'mobile automation should mark offline cached runtime as display-only',
    );
    assertContains(
      controllerSource,
      "runtimeDisplay.snapshot?.remoteStatus === 'local'",
      'mobile automation local success must use local status label',
    );
    assertContains(
      controllerSource,
      'runtimeStatusLocal',
      'mobile automation must reference runtimeStatusLocal for local snapshots',
    );
    assertContains(
      controllerSource,
      'runtimeDisplay.cachedAt !== null',
      'mobile automation cached hint requires warm offline snapshot+cachedAt',
    );
    assertContains(
      controllerSource,
      'selectMobileRuntimeDisplayForProject',
      'mobile automation must isolate runtime display by project on first render',
    );
    assertContains(
      controllerSource,
      'emptyMobileRuntimeDisplayState(true, null, projectId)',
      'project change must synchronously reset runtime display owned by new projectId',
    );
    assertContains(
      controllerSource,
      'OwnedMobileRuntimeDisplayState',
      'mobile runtime display state must carry owning projectId',
    );
    assertContains(
      zhWorkbench,
      '"runtimeStatusOffline": "离线"',
      'zh offline label must be neutral offline without claiming cache',
    );
    assertContains(
      enWorkbench,
      '"runtimeStatusOffline": "Offline"',
      'en offline label must be neutral offline without claiming cache',
    );
    assertContains(
      zhWorkbench,
      '"runtimeStatusLocal": "本机"',
      'zh local runtime label must exist',
    );
    assertContains(
      enWorkbench,
      '"runtimeStatusLocal": "Local"',
      'en local runtime label must exist',
    );
    assertContains(
      controllerSource,
      'loadRuntimeSnapshot(projectId)',
      'mobile automation should refresh runtime snapshot with tasks',
    );
    assertContains(
      workbenchHttpSource,
      "/api/mobile/orchestrator/runtime-snapshot",
      'HTTP transport should call mobile runtime-snapshot route rather than owner P2P base URL',
    );
    assertContains(
      controllerSource,
      'httpOrchestratorTransport.tasks.createView',
      'mobile automation should create local or remote tasks through task view HTTP proxy',
    );
    assertContains(
      controllerSource,
      'splitOrchestratorTaskViews(taskViews)',
      'mobile automation should split task-view data into real tasks and pending remote outbox',
    );
    assertContains(
      controllerSource,
      'MOBILE_AUTOMATION_WORKFLOW_STATES',
      'mobile automation should render compact groups by task.workflowState',
    );
    assertContains(
      controllerSource,
      'groupedTasks[task.task.workflowState].push(task)',
      'mobile automation grouping must use workflowState rather than legacy status',
    );
    assertContains(
      outboxSource,
      'pendingRemoteItems.map',
      'mobile automation pending remote outbox should render in a separate list',
    );
    assertContains(
      controllerSource,
      'setSelectedTaskView(view)',
      'mobile automation should support click-to-expand task details instead of drag interactions',
    );
    assertNotContains(
      panelSource,
      'draggable=',
      'mobile automation must not implement horizontal drag/drop board behavior',
    );
    assertNotContains(
      taskListSource,
      'draggable=',
      'task list must not implement horizontal drag/drop board behavior',
    );
    assertContains(
      controllerSource,
      'httpOrchestratorTransport.tasks.listEvidence',
      'mobile automation details should load task evidence through HTTP transport',
    );
    assertContains(
      taskListSource,
      'lastRuntimeMessage',
      'mobile automation rows should show runtime summary with lastRuntimeMessage',
    );
    assertContains(
      taskListSource,
      'claudeSessionId',
      'mobile automation rows should show Claude session runtime metadata',
    );
    assertContains(
      taskListSource,
      'transcriptPath',
      'mobile automation rows should show transcript runtime metadata',
    );
    assertContains(
      taskDetailSource,
      'blockedReason',
      'mobile automation detail should show blocked reason when available',
    );
    assertContains(
      controllerSource,
      'workbench:mobile.automationPanel.unknown',
      'mobile automation runtime metadata should use localized unknown fallback',
    );
    assertContains(
      controllerSource,
      "createAction: 'backlog'",
      'mobile automation create dialog should support create-to-backlog action',
    );
    assertContains(
      controllerSource,
      "createAction: 'todo'",
      'mobile automation create dialog should support create-to-todo action',
    );
    assertContains(
      controllerSource,
      "createAction: 'start'",
      'mobile automation create dialog should support create-and-start action',
    );
    assertContains(
      workbenchHttpSource,
      'createAction: request.createAction',
      'HTTP createView should send explicit createAction to backend',
    );
    assertContains(
      mobileWorkbenchSource,
      'onOpenExecutionContext={handleOpenAutomationExecutionContext}',
      'MobileWorkbench should let automation details switch to the existing terminal panel',
    );
    assertContains(
      controllerSource,
      'onOpenExecutionContext',
      'MobileAutomationPanel controller should expose open-terminal action through a callback prop',
    );
    assertContains(
      panelSource,
      'MobileAutomationPanelProps',
      'MobileAutomationPanel should re-export panel props type for MobileWorkbench imports',
    );
    assertContains(
      controllerSource,
      'projectId,',
      'mobile AI completion should include projectId so the HTTP route can proxy remote projects',
    );
    assertContains(
      createDialogSource,
      'workbench:mobile.automationPanel.completeWithAi',
      'mobile automation task creation should expose localized AI completion action',
    );
    assertContains(
      stylesSource,
      '.mobileDialogOverlay',
      'mobile automation task creation dialog should have overlay styles',
    );
    assertNotContains(
      panelSource,
      '<form id={formId} className={styles.mobileFormInline} onSubmit={handleSubmit}>',
      'mobile automation task form must not remain fixed inline in the panel',
    );
    assertNotContains(
      panelSource,
      'remoteUnsupported',
      'mobile automation panel should no longer reject remote project shortcuts',
    );
    assertContains(
      zhWorkbench,
      '"completeWithAi": "AI 自动完善"',
      'zh workbench locale should include mobile AI completion copy',
    );
    assertContains(
      zhWorkbench,
      '"createBacklog": "创建到 Backlog"',
      'zh workbench locale should include create backlog action copy',
    );
    assertContains(
      zhWorkbench,
      '"openExecutionContext": "打开执行现场"',
      'zh workbench locale should include open execution context copy',
    );
    assertContains(
      enWorkbench,
      '"completeWithAi": "Fill with AI"',
      'en workbench locale should include mobile AI completion copy',
    );
    assertContains(
      typesSource,
      'externalState: string | null;',
      'mobile automation task DTO should expose tracker externalState',
    );
    assertContains(
      typesSource,
      'externalLabels: string[] | null;',
      'mobile automation task DTO should expose tracker externalLabels',
    );
    assertContains(
      enWorkbench,
      '"createBacklog": "Create in Backlog"',
      'en workbench locale should include create backlog action copy',
    );
    assertContains(
      enWorkbench,
      '"openExecutionContext": "Open execution context"',
      'en workbench locale should include open execution context copy',
    );
  });

  test('runtime cache is display-only and actions derive from task DTO not snapshot', () => {
    const panelSource = readFileSync(
      new URL('./components/MobileAutomationPanel.tsx', import.meta.url),
      'utf8',
    );
    const controllerSource = readFileSync(
      new URL('./controllers/useMobileAutomationController.ts', import.meta.url),
      'utf8',
    );
    const desktopHookSource = readFileSync(
      new URL('../hooks/useOrchestratorRuntimeSnapshot.ts', import.meta.url),
      'utf8',
    );
    const mobileStoreSource = readFileSync(
      new URL('./mobileRuntimeSnapshotStore.ts', import.meta.url),
      'utf8',
    );
    const actionHelperSource = readFileSync(new URL('../lib/orchestrator.ts', import.meta.url), 'utf8');

    assertContains(
      panelSource,
      'runtimeCachedHint',
      'mobile offline cache must be labeled display-only',
    );
    assertNotContains(
      panelSource,
      'localStorage',
      'MobileAutomationPanel must not touch localStorage for runtime cache',
    );
    assertNotContains(
      panelSource,
      'sessionStorage',
      'MobileAutomationPanel must not touch sessionStorage for runtime cache',
    );
    assertNotContains(
      controllerSource,
      'localStorage',
      'controller must not touch localStorage for runtime cache',
    );
    assertNotContains(
      controllerSource,
      'sessionStorage',
      'controller must not touch sessionStorage for runtime cache',
    );
    assertNotContains(
      desktopHookSource,
      'localStorage',
      'desktop runtime hook must not touch localStorage',
    );
    assertNotContains(
      desktopHookSource,
      'sessionStorage',
      'desktop runtime hook must not touch sessionStorage',
    );
    // 注释可提及 localStorage 约束；生产路径不得出现实际 storage API 调用。
    assertNotContains(
      mobileStoreSource,
      'localStorage.',
      'mobile runtime store must not call localStorage APIs',
    );
    assertNotContains(
      mobileStoreSource,
      'sessionStorage',
      'mobile runtime store must not reference sessionStorage',
    );
    // 动作可用性只读 task DTO 字段，参数签名不得出现 runtime snapshot。
    assertContains(
      actionHelperSource,
      'export function canStartOrchestratorTaskForProject(\n  task: OrchestratorTask | null,\n  currentProjectId: string | null | undefined,\n)',
      'canStart must only take task + projectId, not runtime snapshot cache',
    );
    assertContains(
      actionHelperSource,
      'export function canCancelOrchestratorTaskForProject(\n  task: OrchestratorTask | null,\n  currentProjectId: string | null | undefined,\n)',
      'canCancel must only take task + projectId, not runtime snapshot cache',
    );
    assertNotContains(
      actionHelperSource,
      'OrchestratorRuntimeSnapshot',
      'action helpers must not import or consume runtime snapshot types',
    );
    // 面板任务动作走 task view transport；runtime snapshot 只出现在状态条文案，不驱动 create/start。
    assertContains(
      controllerSource,
      'httpOrchestratorTransport.tasks.listViews',
      'actions path continues to use task views transport',
    );
    assertContains(
      controllerSource,
      'httpOrchestratorTransport.tasks.createView',
      'create actions use task createView, not runtime snapshot cache',
    );
    assertNotContains(
      controllerSource,
      'canStartOrchestratorTaskForProject(runtime',
      'start availability must not be computed from runtime snapshot cache',
    );
    // 状态条文案可读取 runtimeDisplay.snapshot；任务动作 gate 不得用 snapshot 真值分支。
    assertNotContains(
      controllerSource,
      'if (runtimeDisplay.snapshot) {',
      'task action gates must not branch on runtimeDisplay.snapshot truthiness',
    );
    assertNotContains(
      controllerSource,
      'canStartOrchestratorTaskForProject(runtimeDisplay.snapshot',
      'start action must not consume runtimeDisplay.snapshot',
    );
  });
});
