import { readFileSync } from 'node:fs';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 自动化视图是项目级入口，回归测试需要在实现偏离“与终端/文件预览同级”时直接失败。
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
 *   自动化页迁入 Workbench 后，代码应保留“独立路由壳”和“可嵌入面板”两个边界，便于旧深链重定向或复用。
 *
 * Code Logic（这个函数做什么）:
 *   读取 Workbench、Orchestrator、AppShell、路由和 i18n 资源，检查自动化 workspace view、嵌入组件、
 *   切换按钮、层级样式和侧栏导航收敛契约。
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

  assertContains(
    workbenchFilesSource,
    "export type WorkbenchFileWorkspaceView = 'terminal' | 'files' | 'automation';",
    'Workbench workspace view union includes automation',
  );
  assertContains(workbenchSource, "from '@/pages/Orchestrator';", 'Workbench imports the Orchestrator panel boundary');
  assertContains(workbenchSource, "setWorkspaceView('automation');", 'Workbench can switch to automation view');
  assertContains(workbenchSource, 'className={styles.automationLayer}', 'Workbench renders an automation layer');
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
  assertContains(workbenchSource, "t('workbench:automationWorkspace.open')", 'terminal toolbar uses localized automation label');
  assertContains(workbenchSource, "t('workbench:automationWorkspace.returnTerminal')", 'automation view has localized return action');
  assertContains(workbenchStyles, '.automationLayer {', 'automation layer has a dedicated style block');
  assertContains(workbenchStyles, '.automationBody {', 'automation layer scroll body exists');
  assertContains(orchestratorSource, 'export function OrchestratorPanel', 'Orchestrator exports an embeddable panel');
  assertContains(orchestratorSource, 'embedded?: boolean;', 'OrchestratorPanel supports embedded mode');
  assertContains(orchestratorSource, 'onOpenWorkbench?: (url: string) => void;', 'OrchestratorPanel lets Workbench own embedded deep-link handling');
  assertContains(appSource, '<Navigate to="/workbench" replace />', 'legacy /orchestrator route redirects to Workbench');
  assertNotContains(appShellSource, 'to="/orchestrator"', 'sidebar no longer exposes a standalone automation nav item');
  assertContains(zhWorkbench, '"automationWorkspace"', 'zh Workbench locale includes automation workspace copy');
  assertContains(enWorkbench, '"automationWorkspace"', 'en Workbench locale includes automation workspace copy');
}

void main();
