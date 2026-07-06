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
  assertContains(workbenchSource, 'className={styles.automationLayer}', 'Workbench renders an automation layer');
  assertContains(
    workbenchSource,
    'data-hidden={!automationConsoleOpen || undefined}',
    'automation layer visibility is driven by project-level console state',
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
    workbenchSource,
    'activeProject?.name',
    'automation context displays the current project name',
  );
  assertContains(
    workbenchSource,
    '<OrchestratorPanel embedded onOpenWorkbench={handleOpenAutomationTaskWorkbench} />',
    'Workbench embeds OrchestratorPanel without page shell and owns task deep-link return',
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
  assertNotContains(
    orchestratorSource,
    'setSelectedTaskId(null);',
    'Remote offline task refresh should not clear the selected task',
  );
  assertNotContains(
    workbenchSource,
    "t('workbench:automationWorkspace.open')",
    'terminal toolbar should no longer expose an automation workspace action',
  );
  assertContains(workbenchSource, "t('workbench:projectAutomation.returnTerminal')", 'automation console has localized return action');
  assertContains(workbenchStyles, '.automationLayer {', 'automation layer has a dedicated style block');
  assertContains(workbenchStyles, '.automationHeader {', 'automation console has a project context header');
  assertContains(workbenchStyles, '.automationBody {', 'automation layer scroll body exists');
  assertContains(orchestratorSource, 'export function OrchestratorPanel', 'Orchestrator exports an embeddable panel');
  assertContains(orchestratorSource, 'embedded?: boolean;', 'OrchestratorPanel supports embedded mode');
  assertContains(orchestratorSource, 'onOpenWorkbench?: (url: string) => void;', 'OrchestratorPanel lets Workbench own embedded deep-link handling');
  assertContains(appSource, '<Navigate to="/workbench" replace />', 'legacy /orchestrator route redirects to Workbench');
  assertNotContains(appShellSource, 'to="/orchestrator"', 'sidebar no longer exposes a standalone automation nav item');
  assertContains(zhWorkbench, '"projectAutomation"', 'zh Workbench locale includes project automation copy');
  assertContains(enWorkbench, '"projectAutomation"', 'en Workbench locale includes project automation copy');
  assertContains(zhWorkbench, '"open": "项目自动化"', 'zh Workbench locale uses project-level automation label');
  assertContains(enWorkbench, '"open": "Project Automation"', 'en Workbench locale uses project-level automation label');
  assertContains(zhOrchestrator, '"activeSession": "执行现场"', 'zh Orchestrator detail avoids terminal-only wording');
  assertContains(enOrchestrator, '"activeSession": "Execution Context"', 'en Orchestrator detail avoids terminal-only wording');
  assertContains(zhOrchestrator, '"openWorkbench": "打开执行现场"', 'zh Orchestrator action opens execution context');
  assertContains(enOrchestrator, '"openWorkbench": "Open execution context"', 'en Orchestrator action opens execution context');
  assertNotContains(zhOrchestrator, '"终端现场"', 'zh Orchestrator copy must not imply task belongs to the current worktree terminal');
  assertNotContains(enOrchestrator, '"Open Workbench"', 'en Orchestrator copy must not imply a generic Workbench jump');
}

void main();
