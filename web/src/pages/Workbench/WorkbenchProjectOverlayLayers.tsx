/**
 * Workbench 项目级控制台层：项目自动化与项目 Agent（条件挂载，不用 hidden）。
 *
 * Business Logic（为什么需要）:
 *   两个控制台互斥且吃满中心工作区；抽到独立 view 避免 Workbench.tsx 超 1200 行，
 *   并用 React.lazy 避免把 Agent Hub 打进 Workbench 主 chunk。
 *
 * Code Logic（做什么）:
 *   打开哪个就挂载哪个；项目 Agent 经 Suspense 懒加载。
 */

import {
  lazy,
  Suspense,
  useLayoutEffect,
  type ReactElement,
  type Ref,
} from 'react';
import { useTranslation } from 'react-i18next';
import { OrchestratorPanel } from '@/pages/Orchestrator';
import type { WorkbenchProjectAgentConsoleHandle } from '@/pages/AgentHub/WorkbenchProjectAgentConsole';
import styles from './Workbench.module.css';

const WorkbenchProjectAgentConsole = lazy(() =>
  import('@/pages/AgentHub/WorkbenchProjectAgentConsole').then((mod) => ({
    default: mod.WorkbenchProjectAgentConsole,
  })),
);

export interface WorkbenchProjectOverlayLayersProps {
  automationOpen: boolean;
  projectAgentOpen: boolean;
  project: {
    id: string;
    name: string;
    kind: string;
    deviceName: string;
  } | null;
  unsavedFiles: boolean;
  projectAgentRef: Ref<WorkbenchProjectAgentConsoleHandle>;
  automationFocusTaskId: string | null;
  automationFocusOutboxId: string | null;
  onOpenAutomationTaskWorkbench: (url: string) => void;
  onFocusTargetNotFound: () => void;
}

/**
 * Business Logic: 控制台必须在打开后才进入文档流，避免 hidden 黑屏。
 * Code Logic: 两个 overlay 互斥由调用方保证；此处只按布尔挂载。
 */
export function WorkbenchProjectOverlayLayers(
  props: WorkbenchProjectOverlayLayersProps,
): ReactElement | null {
  const {
    automationOpen,
    projectAgentOpen,
    project,
    unsavedFiles,
    projectAgentRef,
    automationFocusTaskId,
    automationFocusOutboxId,
    onOpenAutomationTaskWorkbench,
    onFocusTargetNotFound,
  } = props;
  const { t } = useTranslation(['workbench']);

  /**
   * Business Logic: 项目 Agent 由懒加载块挂载，热更新经常留下「项目 {name}」旧节点。
   * Code Logic: 打开控制台后观察层根，删掉冻结项目名行。
   */
  useLayoutEffect(() => {
    if (!projectAgentOpen) return undefined;
    const root = document.querySelector('[data-testid="workbench-project-agent-layer"]');
    if (!root) return undefined;
    const removeStaleProjectName = (): void => {
      root.querySelector('[data-testid="agent-hub-frozen-project"]')?.remove();
    };
    removeStaleProjectName();
    const observer = new MutationObserver(removeStaleProjectName);
    observer.observe(root, { childList: true, subtree: true });
    return () => observer.disconnect();
  }, [projectAgentOpen, project?.id]);

  if (automationOpen) {
    return (
      <div className={styles.automationLayer}>
        <header className={styles.automationHeader}>
          <div className={styles.automationHeadingGroup}>
            <h2 className={styles.automationTitle}>{t('workbench:projectAutomation.title')}</h2>
            <p className={styles.automationDescription}>
              {t('workbench:projectAutomation.description')}
            </p>
          </div>
        </header>
        <div className={styles.automationBody}>
          <OrchestratorPanel
            embedded
            onOpenWorkbench={onOpenAutomationTaskWorkbench}
            focusTaskId={automationFocusTaskId}
            focusOutboxId={automationFocusOutboxId}
            onFocusTargetNotFound={onFocusTargetNotFound}
          />
        </div>
      </div>
    );
  }

  if (projectAgentOpen) {
    return (
      <div className={styles.automationLayer} data-testid="workbench-project-agent-layer">
        <header className={styles.automationHeader}>
          <div className={styles.automationHeadingGroup}>
            <h2 className={styles.automationTitle}>{t('workbench:projectAgent.title')}</h2>
            <p className={styles.automationDescription}>{t('workbench:projectAgent.description')}</p>
          </div>
        </header>
        <div className={styles.automationBody}>
          <Suspense
            fallback={<p className={styles.automationDescription}>{t('workbench:projectAgent.loading')}</p>}
          >
            <WorkbenchProjectAgentConsole
              key={project?.id ?? 'project-agent'}
              ref={projectAgentRef}
              projectKey={project?.id ?? ''}
              unsavedFilesNotice={
                unsavedFiles ? t('workbench:projectAgent.unsavedFilesNotice') : null
              }
            />
          </Suspense>
        </div>
      </div>
    );
  }

  return null;
}
