/**
 * Business Logic（为什么需要）:
 *   移动端自动化工作区需要展示实验组进度与单一决策入口，与桌面合同一致。
 *
 * Code Logic（做什么）:
 *   复用 OrchestratorExperimentPanel 渲染实验列表。
 */
import { OrchestratorExperimentPanel } from '@/pages/Orchestrator/views/OrchestratorExperimentPanel';
import type { OrchestratorExperiment } from '@/lib/types/orchestrator';

export interface MobileAutomationExperimentsProps {
  experiments: OrchestratorExperiment[];
  onApproveRecommended?: (experimentId: string, winnerTaskId: string) => void;
  onCancel?: (experimentId: string) => void;
}

/**
 * Business Logic（为什么需要）:
 *   手机端需要组级实验列表，不展示 Diff。
 *
 * Code Logic（做什么）:
 *   map experiments → OrchestratorExperimentPanel。
 */
export function MobileAutomationExperiments(
  props: MobileAutomationExperimentsProps,
): JSX.Element {
  const { experiments, onApproveRecommended, onCancel } = props;
  if (experiments.length === 0) {
    return <p>暂无实验组</p>;
  }
  return (
    <div>
      {experiments.map((exp) => (
        <OrchestratorExperimentPanel
          key={exp.id}
          experiment={exp}
          onApproveRecommended={onApproveRecommended}
          onCancel={onCancel}
        />
      ))}
    </div>
  );
}
