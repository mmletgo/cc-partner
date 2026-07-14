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
    const mobileWorkbenchSource = readFileSync(new URL('./MobileWorkbench.tsx', import.meta.url), 'utf8');
    const stylesSource = readFileSync(new URL('./MobileWorkbench.module.css', import.meta.url), 'utf8');
    const workbenchHttpSource = readFileSync(new URL('../api/workbenchHttp.ts', import.meta.url), 'utf8');
    const zhWorkbench = readFileSync(new URL('../i18n/locales/zh/workbench.json', import.meta.url), 'utf8');
    const enWorkbench = readFileSync(new URL('../i18n/locales/en/workbench.json', import.meta.url), 'utf8');
    const typesSource = readFileSync(new URL('../lib/types/orchestrator.ts', import.meta.url), 'utf8');

    assertContains(
      panelSource,
      'const [createDialogOpen, setCreateDialogOpen] = useState<boolean>(false);',
      'mobile automation task creation should use dialog open state',
    );
    assertContains(panelSource, '<Dialog', 'mobile automation task creation should render shared Dialog');
    assertContains(
      panelSource,
      "from '@/components/primitives'",
      'mobile automation should import Dialog from primitives',
    );
    assertContains(panelSource, 'titleId={dialogTitleId}', 'mobile automation Dialog should wire titleId');
    assertContains(
      panelSource,
      'closeOnEscape={!(creating || completingPrompt)}',
      'mobile automation Dialog should block Escape while creating/completing',
    );
    assertContains(
      panelSource,
      'closeOnBackdrop={!(creating || completingPrompt)}',
      'mobile automation Dialog should block backdrop close while creating/completing',
    );
    assertContains(
      panelSource,
      'initialFocusRef={promptDraftRef}',
      'mobile automation Dialog should focus short prompt on open',
    );
    assertNotContains(
      panelSource,
      'role="dialog"',
      'mobile automation should not hand-write dialog role',
    );
    assertNotContains(
      panelSource,
      'aria-modal="true"',
      'mobile automation should not hand-write aria-modal',
    );
    assertNotContains(
      panelSource,
      "window.addEventListener('keydown'",
      'mobile automation Escape should be owned by Dialog, not local listener',
    )
    assertContains(
      panelSource,
      'httpOrchestratorTransport.tasks.completePrompt',
      'mobile automation task creation should call AI prompt completion through HTTP transport',
    );
    assertContains(
      panelSource,
      'httpOrchestratorTransport.tasks.listViews',
      'mobile automation should list local or remote tasks through task view HTTP proxy',
    );
    assertContains(
      panelSource,
      'httpOrchestratorTransport.outbox.retry',
      'mobile automation should retry failed outbox through local HTTP route',
    );
    assertContains(
      panelSource,
      'httpOrchestratorTransport.outbox.discard',
      'mobile automation should discard failed outbox through local HTTP route',
    );
    assertContains(
      panelSource,
      "item.status === 'failed'",
      'mobile outbox actions should render only for failed status',
    );
    assertContains(
      panelSource,
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
      panelSource,
      'httpOrchestratorTransport.getRuntimeSnapshot',
      'mobile automation should load remote-aware runtime snapshot through mobile HTTP route',
    );
    assertContains(
      panelSource,
      'applyMobileRuntimeSnapshotSuccess',
      'mobile automation should apply in-memory runtime snapshot cache store',
    );
    assertContains(
      panelSource,
      'toRuntimeLoadError',
      'mobile automation runtime load catch must preserve transport Error kind via toRuntimeLoadError',
    );
    assertNotContains(
      panelSource,
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
      panelSource,
      "runtimeDisplay.snapshot?.remoteStatus === 'local'",
      'mobile automation local success must use local status label',
    );
    assertContains(
      panelSource,
      'runtimeStatusLocal',
      'mobile automation must reference runtimeStatusLocal for local snapshots',
    );
    assertContains(
      panelSource,
      'runtimeDisplay.cachedAt !== null',
      'mobile automation cached hint requires warm offline snapshot+cachedAt',
    );
    assertContains(
      panelSource,
      'selectMobileRuntimeDisplayForProject',
      'mobile automation must isolate runtime display by project on first render',
    );
    assertContains(
      panelSource,
      'emptyMobileRuntimeDisplayState(true, null, projectId)',
      'project change must synchronously reset runtime display owned by new projectId',
    );
    assertContains(
      panelSource,
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
      panelSource,
      'loadRuntimeSnapshot(projectId)',
      'mobile automation should refresh runtime snapshot with tasks',
    );
    assertContains(
      workbenchHttpSource,
      "/api/mobile/orchestrator/runtime-snapshot",
      'HTTP transport should call mobile runtime-snapshot route rather than owner P2P base URL',
    );
    assertContains(
      panelSource,
      'httpOrchestratorTransport.tasks.createView',
      'mobile automation should create local or remote tasks through task view HTTP proxy',
    );
    assertContains(
      panelSource,
      'splitOrchestratorTaskViews(taskViews)',
      'mobile automation should split task-view data into real tasks and pending remote outbox',
    );
    assertContains(
      panelSource,
      'MOBILE_AUTOMATION_WORKFLOW_STATES',
      'mobile automation should render compact groups by task.workflowState',
    );
    assertContains(
      panelSource,
      'groupedTasks[task.task.workflowState].push(task)',
      'mobile automation grouping must use workflowState rather than legacy status',
    );
    assertContains(
      panelSource,
      'pendingRemoteItems.map',
      'mobile automation pending remote outbox should render in a separate list',
    );
    assertContains(
      panelSource,
      'setSelectedTaskView(view)',
      'mobile automation should support click-to-expand task details instead of drag interactions',
    );
    assertNotContains(
      panelSource,
      'draggable=',
      'mobile automation must not implement horizontal drag/drop board behavior',
    );
    assertContains(
      panelSource,
      'httpOrchestratorTransport.tasks.listEvidence',
      'mobile automation details should load task evidence through HTTP transport',
    );
    assertContains(
      panelSource,
      'lastRuntimeMessage',
      'mobile automation rows should show runtime summary with lastRuntimeMessage',
    );
    assertContains(
      panelSource,
      'claudeSessionId',
      'mobile automation rows should show Claude session runtime metadata',
    );
    assertContains(
      panelSource,
      'transcriptPath',
      'mobile automation rows should show transcript runtime metadata',
    );
    assertContains(
      panelSource,
      'blockedReason',
      'mobile automation detail should show blocked reason when available',
    );
    assertContains(
      panelSource,
      'workbench:mobile.automationPanel.unknown',
      'mobile automation runtime metadata should use localized unknown fallback',
    );
    assertContains(
      panelSource,
      "createAction: 'backlog'",
      'mobile automation create dialog should support create-to-backlog action',
    );
    assertContains(
      panelSource,
      "createAction: 'todo'",
      'mobile automation create dialog should support create-to-todo action',
    );
    assertContains(
      panelSource,
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
      panelSource,
      'onOpenExecutionContext',
      'MobileAutomationPanel should expose open-terminal action through a callback prop',
    );
    assertContains(
      panelSource,
      'projectId,',
      'mobile AI completion should include projectId so the HTTP route can proxy remote projects',
    );
    assertContains(
      panelSource,
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
      panelSource,
      'httpOrchestratorTransport.tasks.listViews',
      'actions path continues to use task views transport',
    );
    assertContains(
      panelSource,
      'httpOrchestratorTransport.tasks.createView',
      'create actions use task createView, not runtime snapshot cache',
    );
    assertNotContains(
      panelSource,
      'canStartOrchestratorTaskForProject(runtime',
      'start availability must not be computed from runtime snapshot cache',
    );
    // 状态条文案可读取 runtimeDisplay.snapshot；任务动作 gate 不得用 snapshot 真值分支。
    assertNotContains(
      panelSource,
      'if (runtimeDisplay.snapshot) {',
      'task action gates must not branch on runtimeDisplay.snapshot truthiness',
    );
    assertNotContains(
      panelSource,
      'canStartOrchestratorTaskForProject(runtimeDisplay.snapshot',
      'start action must not consume runtimeDisplay.snapshot',
    );

  });
});
