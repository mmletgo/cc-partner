import { readFileSync } from 'node:fs';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 自动化入口必须保持项目级语境，回归测试需要在它重新混入终端工具栏或 worktree 视图时失败。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛出 Error，让 tsx 测试进程以非零状态退出。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   静态契约测试需要确认关键源码片段存在，避免自动化重新退回独立主页面。
 *
 * Code Logic（这个函数做什么）:
 *   检查源码包含指定字符串；缺失时抛出带上下文的错误。
 */
function assertContains(source: string, expected: string, message: string): void {
  assert(source.includes(expected), message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   侧栏主导航不应继续暴露独立自动化入口，避免用户绕过项目 Workbench 上下文。
 *
 * Code Logic（这个函数做什么）:
 *   检查源码不包含指定字符串；存在时抛出带上下文的错误。
 */
function assertNotContains(source: string, unexpected: string, message: string): void {
  assert(!source.includes(unexpected), message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   自动化页迁入 Workbench 后，代码应保留“独立路由壳”和“可嵌入面板”两个边界，并明确 Workbench 侧是项目级控制台。
 *
 * Code Logic（这个函数做什么）:
 *   读取 Workbench、Orchestrator、AppShell、路由和 i18n 资源，检查自动化控制台入口、嵌入组件、
 *   deep link 回终端、层级样式、侧栏导航收敛和执行现场文案契约。
 */
async function main(): Promise<void> {
  const workbenchSource = readFileSync(new URL('./Workbench.tsx', import.meta.url), 'utf8');
  const workbenchFilesSource = readFileSync(new URL('./workbenchFiles.ts', import.meta.url), 'utf8');
  const workbenchStyles = readFileSync(new URL('./Workbench.module.css', import.meta.url), 'utf8');
  const orchestratorSource = readFileSync(new URL('../Orchestrator/Orchestrator.tsx', import.meta.url), 'utf8');
  const orchestratorStyles = readFileSync(new URL('../Orchestrator/Orchestrator.module.css', import.meta.url), 'utf8');
  const orchestratorLibSource = readFileSync(new URL('../../lib/orchestrator.ts', import.meta.url), 'utf8');
  const typesSource = readFileSync(new URL('../../lib/types.ts', import.meta.url), 'utf8');
  const appShellSource = readFileSync(
    new URL('../../components/layout/AppShell/AppShell.tsx', import.meta.url),
    'utf8',
  );
  const appSource = readFileSync(new URL('../../App.tsx', import.meta.url), 'utf8');
  const zhWorkbench = readFileSync(new URL('../../i18n/locales/zh/workbench.json', import.meta.url), 'utf8');
  const enWorkbench = readFileSync(new URL('../../i18n/locales/en/workbench.json', import.meta.url), 'utf8');
  const zhOrchestrator = readFileSync(new URL('../../i18n/locales/zh/orchestrator.json', import.meta.url), 'utf8');
  const enOrchestrator = readFileSync(new URL('../../i18n/locales/en/orchestrator.json', import.meta.url), 'utf8');

  assertContains(
    workbenchFilesSource,
    "export type WorkbenchFileWorkspaceView = 'terminal' | 'files';",
    'Workbench workspace view union stays limited to terminal/files',
  );
  assertNotContains(
    workbenchFilesSource,
    "'automation'",
    'Project automation must not be modeled as a file/terminal workspace view',
  );
  assertContains(workbenchSource, "from '@/pages/Orchestrator';", 'Workbench imports the Orchestrator panel boundary');
  assertContains(
    workbenchSource,
    'const [automationConsoleOpen, setAutomationConsoleOpen] = useState<boolean>(false);',
    'Workbench owns a project-level automation console state separate from workspaceView',
  );
  assertContains(
    workbenchSource,
    'setAutomationConsoleOpen(true);',
    'Workbench can open the project automation console',
  );
  assertContains(
    workbenchSource,
    'const handleToggleProjectAutomation = useCallback',
    'Workbench project automation header action is a toggle, not a one-way open action',
  );
  assertContains(
    workbenchSource,
    'if (automationConsoleOpen)',
    'project automation toggle detects the open state before returning to terminal',
  );
  assertContains(
    workbenchSource,
    "setWorkspaceView('terminal');",
    'project automation toggle always returns the center workspace to terminal context',
  );
  assertContains(
    workbenchSource,
    'onClick={handleToggleProjectAutomation}',
    'clicking the project automation button again toggles back to the terminal page',
  );
  assertNotContains(
    workbenchSource,
    "setWorkspaceView('automation');",
    'Workbench must not switch the file/terminal workspace view to automation',
  );
  assertNotContains(
    workbenchSource,
    "workspaceView === 'automation'",
    'Workbench must not check automation through workspaceView equality',
  );
  assertNotContains(
    workbenchSource,
    "workspaceView !== 'automation'",
    'Workbench must not hide automation through workspaceView inequality',
  );
  assertContains(
    workbenchSource,
    '{automationConsoleOpen ? (',
    'Workbench should mount the automation layer only after the project automation console is open',
  );
  assertContains(workbenchSource, '<div className={styles.automationLayer}>', 'Workbench renders an automation layer');
  assertNotContains(
    workbenchSource,
    'data-hidden={!automationConsoleOpen || undefined}',
    'automation layer must not stay in the DOM behind a hidden/data-hidden style that can leave a black workspace',
  );
  assertContains(
    workbenchSource,
    '<OrchestratorPanel embedded onOpenWorkbench={handleOpenAutomationTaskWorkbench} />',
    'mounted automation layer should contain the embedded Orchestrator UI',
  );
  assertNotContains(
    workbenchSource,
    'hidden={!automationConsoleOpen}',
    'automation layer must not rely on the HTML hidden attribute because it can keep the opened console display:none',
  );
  assertContains(
    workbenchSource,
    "t('workbench:projectAutomation.open')",
    'Workbench header uses localized Project Automation entry',
  );
  assertContains(
    workbenchSource,
    "t('workbench:projectAutomation.scope')",
    'Workbench header states the automation scope is project-level',
  );
  assertContains(
    workbenchSource,
    "t('workbench:projectAutomation.scopeValue'",
    'automation panel context renders localized project scope value',
  );
  assertContains(
    workbenchSource,
    'hidden={automationConsoleOpen}',
    'project automation console hides the worktree switcher to avoid worktree ownership ambiguity',
  );
  assertContains(
    workbenchStyles,
    '.worktreeBar[hidden]',
    'worktree switcher hidden attribute must win over the author display:flex style',
  );
  assertNotContains(
    workbenchStyles,
    '.automationLayer[hidden]',
    'automation layer is conditionally mounted, so it should not need a hidden-attribute CSS override',
  );
  assertNotContains(
    workbenchStyles,
    '.automationLayer[data-hidden',
    'automation layer is conditionally mounted, so it should not have a data-hidden CSS path',
  );
  assertContains(
    workbenchSource,
    'activeProject?.name',
    'automation context displays the current project name',
  );
  assertContains(
    workbenchSource,
    'const handleOpenAutomationTaskWorkbench = useCallback',
    'Workbench switches back to terminal when automation task opens its workbench context',
  );
  assertContains(
    orchestratorSource,
    'buildWorkbenchDeepLink({',
    'Orchestrator task links use the shared Workbench deep link builder',
  );
  assertContains(
    orchestratorSource,
    'views: current?.projectId === projectId ? current.views : []',
    'Orchestrator preserves the selected task list when a remote refresh fails',
  );
  assertContains(
    orchestratorSource,
    "orchestratorTaskProgressMessage(selectedTaskView, t)",
    'Orchestrator task detail renders progress copy for running/verifying/repairing attempts',
  );
  assertContains(
    orchestratorSource,
    'return tasks.find((item) => item.task.id === selectedTaskId) ?? null;',
    'Orchestrator must not auto-select the first task before the user clicks a card',
  );
  assertContains(
    orchestratorSource,
    '<OrchestratorDialogPortal>',
    'Orchestrator task detail and evidence should render in a body-level side panel',
  );
  assertContains(
    orchestratorSource,
    'className={styles.taskDrawerOverlay}',
    'Orchestrator task detail opens from the board as a side drawer overlay',
  );
  assertContains(
    orchestratorSource,
    'const handleCloseTaskDrawer = useCallback',
    'Orchestrator task detail drawer has an explicit close path',
  );
  assertNotContains(
    orchestratorSource,
    '?? tasks[0] ?? null',
    'Orchestrator must not fall back to the first task as the selected detail task',
  );
  assertNotContains(
    orchestratorSource,
    'if (createdTaskId) setSelectedTaskId(createdTaskId);',
    'Creating a task must update the board without opening the detail drawer automatically',
  );
  assertContains(
    orchestratorLibSource,
    'if (!currentSelectedTaskId) return null;',
    'Async task actions must not reopen the detail drawer after the user closed it',
  );
  assertContains(
    orchestratorSource,
    'ORCHESTRATOR_BOARD_LANES',
    'Orchestrator queue renders the workflow board lane order from orchestratorBoard helpers',
  );
  assertContains(
    orchestratorSource,
    'groupRenderableTasksByWorkflowState(tasks)',
    'Orchestrator queue groups renderable tasks by workflow state',
  );
  assertContains(
    orchestratorSource,
    'canMoveRenderableTaskToWorkflowState',
    'Orchestrator queue uses the workflow move guard before enabling drag/drop',
  );
  assertContains(
    orchestratorSource,
    'orchestratorApi.moveTaskWorkflowState(',
    'Orchestrator drag/drop calls the workflow state move API',
  );
  assertContains(
    orchestratorSource,
    'orchestratorApi.getRuntimeSnapshot(',
    'Orchestrator panel loads the local project runtime snapshot',
  );
  assertContains(
    typesSource,
    'latestTickAt: string | null;',
    'runtime snapshot type includes scheduler latest tick time',
  );
  assertContains(
    typesSource,
    'runningTasks: OrchestratorRuntimeTaskSummary[];',
    'runtime snapshot type includes running task summaries',
  );
  assertContains(
    typesSource,
    'retryingTasks: OrchestratorRuntimeTaskSummary[];',
    'runtime snapshot type includes retrying task summaries',
  );
  assertContains(
    typesSource,
    'recentEvents: OrchestratorRuntimeEvent[];',
    'runtime snapshot type includes recent scheduler/runner events',
  );
  assertContains(
    typesSource,
    "remoteStatus: 'local' | 'unsupported' | 'unavailable' | 'offline';",
    'runtime snapshot type includes explicit local/remote status',
  );
  assertContains(
    orchestratorSource,
    "icon={<RefreshIcon />}",
    'runtime snapshot status strip exposes a manual refresh button',
  );
  assertContains(
    orchestratorSource,
    "navigate('/settings?tab=automation');",
    'runtime snapshot status strip links to Settings automation tab',
  );
  assertContains(
    orchestratorSource,
    'activeRuntimeSnapshotResult.snapshot.latestTickAt',
    'runtime snapshot status strip renders latest scheduler tick time',
  );
  assertContains(
    orchestratorSource,
    'activeRuntimeSnapshotResult.snapshot.generatedAt',
    'runtime snapshot status strip renders snapshot generated time',
  );
  assertContains(
    orchestratorSource,
    'activeRuntimeSnapshotResult.snapshot.runningTasks.length',
    'runtime snapshot status strip renders running task count',
  );
  assertContains(
    orchestratorSource,
    'activeRuntimeSnapshotResult.snapshot.recentEvents.map',
    'runtime snapshot status strip renders recent event summaries',
  );
  assertContains(
    zhOrchestrator,
    '"refresh": "刷新状态"',
    'zh Orchestrator locale includes snapshot refresh copy',
  );
  assertContains(
    enOrchestrator,
    '"refresh": "Refresh status"',
    'en Orchestrator locale includes snapshot refresh copy',
  );
  assertContains(
    zhOrchestrator,
    '"settings": "自动化设置"',
    'zh Orchestrator locale includes Settings automation link copy',
  );
  assertContains(
    enOrchestrator,
    '"settings": "Automation settings"',
    'en Orchestrator locale includes Settings automation link copy',
  );
  assertContains(
    zhOrchestrator,
    '"remoteUnavailable": "远端运行时快照暂不可用；请在所属设备查看自动化状态。"',
    'zh Orchestrator locale uses explicit remote snapshot unavailable copy',
  );
  assertContains(
    enOrchestrator,
    '"remoteUnavailable": "Remote runtime snapshot is unavailable here. Open the owning device to inspect automation state."',
    'en Orchestrator locale uses explicit remote snapshot unavailable copy',
  );
  assertNotContains(
    orchestratorSource,
    'ORCHESTRATOR_STATUSES.map((status)',
    'Orchestrator main board must not render lanes from legacy task statuses',
  );
  assertNotContains(
    orchestratorSource,
    'groupOrchestratorRenderableTasks(tasks)',
    'Orchestrator main board must not group queue cards by legacy task status',
  );
  assertContains(
    orchestratorSource,
    'const [createDialogOpen, setCreateDialogOpen] = useState<boolean>(false);',
    'Orchestrator task creation should be opened from a modal dialog state instead of a fixed page card',
  );
  assertContains(
    orchestratorSource,
    'role="dialog"',
    'Orchestrator task creation should render as an accessible dialog',
  );
  assertContains(
    orchestratorSource,
    "import { createPortal } from 'react-dom';",
    'desktop Orchestrator task creation dialog should use a body portal to escape Workbench scroll and terminal layers',
  );
  assertContains(
    orchestratorSource,
    'createPortal(',
    'desktop Orchestrator task creation dialog should render through a React portal',
  );
  assertContains(
    orchestratorSource,
    'document.body',
    'desktop Orchestrator task creation dialog should render at document.body level',
  );
  assertContains(
    orchestratorSource,
    'promptOptimizerApi.completeOrchestratorTaskPrompt',
    'Orchestrator task creation should let AI complete title, goal, and acceptance criteria from a short prompt',
  );
  assertContains(
    orchestratorStyles,
    '.createDialogOverlay',
    'Orchestrator task creation dialog should have an overlay style boundary',
  );
  assertContains(
    orchestratorStyles,
    'z-index: calc(var(--z-modal) + 1);',
    'Orchestrator task creation dialog overlay should sit above Workbench terminal fullscreen modal layer',
  );
  assertNotContains(
    orchestratorSource,
    'className={styles.createCard}',
    'Orchestrator task creation form must not stay as a fixed card in the page grid',
  );
  assertContains(
    orchestratorSource,
    "orchestratorEvidenceKindLabel(item.kind, t)",
    'Orchestrator evidence renders localized evidence kind labels',
  );
  assertNotContains(
    orchestratorSource,
    'getProjectConfig',
    'Workbench automation view should not load project-scoped Orchestrator policy',
  );
  assertNotContains(
    orchestratorSource,
    "t('orchestrator:policy.",
    'Workbench automation view should not render the legacy project policy card',
  );
  assertContains(
    orchestratorSource,
    'if (current && nextSplit.tasks.some((item) => item.task.id === current)) return current;',
    'Remote offline task refresh preserves an existing selected task when that task is still present',
  );
  assertContains(
    orchestratorSource,
    `setSelectedTaskId((current) => {
          if (current && nextSplit.tasks.some((item) => item.task.id === current)) return current;
          return null;
        });`,
    'Task refresh without an existing selected task leaves the detail drawer closed',
  );
  assertNotContains(
    workbenchSource,
    "t('workbench:automationWorkspace.open')",
    'terminal toolbar should no longer expose an automation workspace action',
  );
  assertNotContains(
    workbenchSource,
    "t('workbench:projectAutomation.returnTerminal')",
    'automation console should rely on the header Project Automation toggle instead of a secondary return button',
  );
  assertContains(workbenchStyles, '.automationLayer {', 'automation layer has a dedicated style block');
  assertContains(workbenchStyles, '.automationHeader {', 'automation console has a project context header');
  assertContains(workbenchStyles, '.automationBody {', 'automation layer scroll body exists');
  assertContains(orchestratorSource, 'export function OrchestratorPanel', 'Orchestrator exports an embeddable panel');
  assertContains(orchestratorSource, 'embedded?: boolean;', 'OrchestratorPanel supports embedded mode');
  assertContains(orchestratorSource, 'onOpenWorkbench?: (url: string) => void;', 'OrchestratorPanel lets Workbench own embedded deep-link handling');
  assertContains(
    orchestratorStyles,
    'height: 100%;',
    'embedded Orchestrator panel should fill the Workbench automation body instead of collapsing to an empty black area',
  );
  assertContains(
    orchestratorStyles,
    '.embedded .grid',
    'embedded Orchestrator grid should have a Workbench-specific layout boundary',
  );
  assertContains(appSource, '<Navigate to="/workbench" replace />', 'legacy /orchestrator route redirects to Workbench');
  assertNotContains(appShellSource, 'to="/orchestrator"', 'sidebar no longer exposes a standalone automation nav item');
  assertContains(zhWorkbench, '"projectAutomation"', 'zh Workbench locale includes project automation copy');
  assertContains(enWorkbench, '"projectAutomation"', 'en Workbench locale includes project automation copy');
  assertContains(zhWorkbench, '"open": "项目自动化"', 'zh Workbench locale uses project-level automation label');
  assertContains(enWorkbench, '"open": "Project Automation"', 'en Workbench locale uses project-level automation label');
  assertContains(zhOrchestrator, '"activeSession": "执行现场"', 'zh Orchestrator detail avoids terminal-only wording');
  assertContains(enOrchestrator, '"activeSession": "Execution Context"', 'en Orchestrator detail avoids terminal-only wording');
  assertContains(zhOrchestrator, '"drawerAria": "任务详情侧边栏"', 'zh Orchestrator detail drawer has accessible copy');
  assertContains(enOrchestrator, '"drawerAria": "Task detail side panel"', 'en Orchestrator detail drawer has accessible copy');
  assertContains(zhOrchestrator, '"openWorkbench": "打开执行现场"', 'zh Orchestrator action opens execution context');
  assertContains(enOrchestrator, '"openWorkbench": "Open execution context"', 'en Orchestrator action opens execution context');
  assertNotContains(zhOrchestrator, '"终端现场"', 'zh Orchestrator copy must not imply task belongs to the current worktree terminal');
  assertNotContains(enOrchestrator, '"Open Workbench"', 'en Orchestrator copy must not imply a generic Workbench jump');
}

void main();
