/**
 * Workbench 标题区：项目路径、Agent 历史、项目 Agent 与项目自动化开关。
 *
 * Business Logic（为什么需要）:
 *   项目 Agent 与项目自动化共用标题栏入口且互斥；抽到独立 view 让 Workbench.tsx 保持 ≤1200 行。
 *
 * Code Logic（做什么）:
 *   渲染 workspaceHeader：title + 标语空隙 + ledger/项目 Agent/项目自动化。
 */

import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/primitives';
import { ClaudeMdIcon, OrchestratorIcon } from '@/lib/icons';
import { AgentLedgerWorkbenchChrome } from './views/AgentLedgerWorkbenchChrome';
import { WorkbenchBanner } from './views/WorkbenchBanner';
import { WorkbenchBatteryBadge } from './views/WorkbenchBatteryBadge';
import type { WorkbenchProjectControllerResult } from './controllers/useWorkbenchProjectController';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
import { useExperimentalFeatures } from '@/hooks/useExperimentalFeatures';
import styles from './Workbench.module.css';

export interface WorkbenchWorkspaceHeaderProps {
  workspaceLine: string;
  terminalFullscreen: boolean;
  activeProjectId: string | null;
  projectCtrl: WorkbenchProjectControllerResult;
  projectAgentOpen: boolean;
  automationOpen: boolean;
  onToggleProjectAgent: () => void;
  onToggleAutomation: () => void;
}

/**
 * Business Logic: 标题栏是项目级控制台的唯一入口，不进终端/文件工具栏。
 * Code Logic: 组合 title + ledger + 两个 toggle；不持有 overlay 内容。
 */
export function WorkbenchWorkspaceHeader(props: WorkbenchWorkspaceHeaderProps): ReactElement {
  const {
    workspaceLine,
    terminalFullscreen,
    activeProjectId,
    projectCtrl,
    projectAgentOpen,
    automationOpen,
    onToggleProjectAgent,
    onToggleAutomation,
  } = props;
  const { t } = useTranslation(['workbench']);
  const { features } = useExperimentalFeatures();
  const { activeProject } = useWorkbenchProjects();
  const bannerDeviceId =
    activeProject?.kind === 'remote' ? activeProject.deviceId : undefined;

  return (
    <section className={styles.workspaceHeader}>
      <div className={styles.workspaceTitleGroup}>
        <div>
          <div className={styles.workspaceTitleRow}>
            <h1 className={styles.workspaceTitle}>{t('workbench:title')}</h1>
            <WorkbenchBatteryBadge />
          </div>
          <p className={styles.workspacePath}>{workspaceLine}</p>
        </div>
      </div>
      <WorkbenchBanner
        deviceId={bannerDeviceId}
        remoteWriteDisabled={projectCtrl.remoteWriteDisabled}
      />
      <div className={styles.workspaceHeaderActions}>
        <AgentLedgerWorkbenchChrome
          showTrigger={!terminalFullscreen}
          disabled={!activeProjectId}
          open={projectCtrl.agentLedgerOpen}
          localOnlyAvailable={projectCtrl.agentLedgerLocalOnly}
          page={projectCtrl.agentLedgerPage}
          summary={projectCtrl.agentLedgerSummary}
          loading={projectCtrl.agentLedgerLoading}
          loadingMore={projectCtrl.agentLedgerLoadingMore}
          error={projectCtrl.agentLedgerError}
          onOpen={projectCtrl.openAgentLedger}
          onClose={projectCtrl.closeAgentLedger}
          onLoadMore={() => void projectCtrl.loadMoreAgentLedger()}
          onRefresh={() => void projectCtrl.refreshAgentLedger()}
        />
        {terminalFullscreen ? null : (
          <Button
            className={styles.projectAutomationButton}
            variant="secondary"
            size="sm"
            icon={<ClaudeMdIcon />}
            title={t('workbench:projectAgent.description')}
            aria-label={t('workbench:projectAgent.open')}
            aria-pressed={projectAgentOpen}
            data-active={projectAgentOpen || undefined}
            data-testid="workbench-project-agent-toggle"
            disabled={!activeProjectId}
            onClick={onToggleProjectAgent}
          >
            {t('workbench:projectAgent.open')}
          </Button>
        )}
        {features.automation ? (
          <Button
            className={styles.projectAutomationButton}
            variant="secondary"
            size="sm"
            icon={<OrchestratorIcon />}
            title={t('workbench:projectAutomation.description')}
            aria-label={t('workbench:projectAutomation.open')}
            aria-pressed={automationOpen}
            data-active={automationOpen || undefined}
            disabled={!activeProjectId}
            onClick={onToggleAutomation}
          >
            {t('workbench:projectAutomation.open')}
          </Button>
        ) : null}
      </div>
    </section>
  );
}
