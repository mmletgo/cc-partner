/**
 * Business Logic（为什么需要）:
 *   实验组只展示组级进度、candidate 状态与推荐理由；NeedsDecision 时提供采用推荐/取消，
 *   绝不展示 Diff/批注。
 *
 * Code Logic（做什么）:
 *   纯展示组件：接收 experiment DTO 与动作回调。
 */
import { Button } from '@/components/primitives/Button';
import type { OrchestratorExperiment } from '@/lib/types/orchestrator';
import styles from './OrchestratorExperimentPanel.module.css';

export interface OrchestratorExperimentPanelProps {
  experiment: OrchestratorExperiment;
  onApproveRecommended?: (experimentId: string, winnerTaskId: string) => void;
  onSelectCandidate?: (experimentId: string, taskId: string) => void;
  onCancel?: (experimentId: string) => void;
}

/**
 * Business Logic（为什么需要）:
 *   自动化实验详情需要单一决策入口，避免 N 个 candidate 各自 Human Review。
 *
 * Code Logic（做什么）:
 *   渲染状态、candidates、推荐理由；NeedsDecision 显示采用推荐/选择/取消。
 */
export function OrchestratorExperimentPanel(
  props: OrchestratorExperimentPanelProps,
): JSX.Element {
  const { experiment, onApproveRecommended, onSelectCandidate, onCancel } = props;
  const needsDecision = experiment.status === 'needsDecision' || experiment.status === 'winnerReady';
  const readyCandidates = (experiment.candidates ?? []).filter(
    (c) => c.outcome === 'candidateReady' || c.outcome === 'winner',
  );
  const recommendedId = experiment.winnerTaskId ?? readyCandidates[0]?.taskId ?? null;

  return (
    <section className={styles.panel} aria-label={experiment.title}>
      <header className={styles.header}>
        <h3 className={styles.title}>{experiment.title}</h3>
        <span className={styles.status}>{experiment.status}</span>
      </header>
      <p className={styles.goal}>{experiment.goal}</p>
      {experiment.selectionReason ? (
        <p className={styles.reason} data-testid="experiment-reason">
          {experiment.selectionReason}
        </p>
      ) : null}
      <ul className={styles.candidates}>
        {(experiment.candidates ?? []).map((c) => (
          <li key={c.taskId} className={styles.candidate}>
            <span>
              #{c.ordinal} {c.strategyLabel} · {c.providerId}
            </span>
            <span>{c.outcome}</span>
            {needsDecision &&
            (c.outcome === 'candidateReady' || c.outcome === 'winner') &&
            onSelectCandidate &&
            c.taskId !== recommendedId ? (
              <Button
                variant="ghost"
                size="sm"
                onClick={() => onSelectCandidate(experiment.id, c.taskId)}
              >
                选择此候选
              </Button>
            ) : null}
          </li>
        ))}
      </ul>
      {needsDecision ? (
        <div className={styles.actions}>
          {recommendedId && onApproveRecommended ? (
            <Button
              variant="primary"
              size="sm"
              onClick={() => onApproveRecommended(experiment.id, recommendedId)}
            >
              采用推荐
            </Button>
          ) : null}
          {onCancel ? (
            <Button variant="danger" size="sm" onClick={() => onCancel(experiment.id)}>
              取消实验
            </Button>
          ) : null}
        </div>
      ) : null}
    </section>
  );
}
