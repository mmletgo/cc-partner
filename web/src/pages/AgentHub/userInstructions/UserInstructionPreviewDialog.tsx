import type { JSX } from 'react';
import { Button, Dialog, Pill, StatusMessage } from '@/components/primitives';
import type { UserInstructionPlanDto } from '@/lib/types/agentHub';
import type { TFunction } from 'i18next';
import styles from '../AgentHub.module.css';

export interface UserInstructionPreviewDialogProps {
  t: TFunction<['agentHub', 'common']>;
  open: boolean;
  busy: boolean;
  plan: UserInstructionPlanDto | null;
  error: string | null;
  onClose: () => void;
  onApply: () => void;
}

/**
 * Business Logic（为什么需要）:
 *   所有 canonical/binding/ownership/file 改动前必须展示 target 路径、操作、diff 和优先级影响。
 *
 * Code Logic（做什么）:
 *   逐 change 渲染路径级 plan；1 MiB hard limit 导致 truncated 时明确提示，blocking 时禁用 apply。
 */
export function UserInstructionPreviewDialog(props: UserInstructionPreviewDialogProps): JSX.Element {
  const { t, open, busy, plan, error, onClose, onApply } = props;
  const actionableCount =
    plan?.changes.filter((change) => change.operation !== 'leave' && !change.emptyDueToTargetOnly)
      .length ?? 0;
  const blocked = Boolean(plan?.blockingReasons.length || !actionableCount);
  return (
    <Dialog
      open={open}
      titleId="user-instruction-preview-title"
      onClose={onClose}
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
      className={styles.userPreviewSurface}
    >
      <div className={styles.userDialogBody} data-testid="user-instruction-preview-dialog">
        <header className={styles.userDialogHeader}>
          <h2 id="user-instruction-preview-title" className={styles.userDialogTitle}>
            {t('agentHub:userInstructions.plan.title')}
          </h2>
          <p className={styles.userSectionDescription}>
            {t('agentHub:userInstructions.plan.description')}
          </p>
        </header>
        {error ? <StatusMessage tone="danger">{error}</StatusMessage> : null}
        {plan?.truncated ? (
          <StatusMessage tone="warn" data-testid="user-instruction-plan-truncated">
            {t('agentHub:userInstructions.plan.truncated')}
          </StatusMessage>
        ) : null}
        {plan?.blockingReasons.length ? (
          <StatusMessage tone="warn">
            <span>{t('agentHub:userInstructions.plan.blocked')}</span>
            <ul className={styles.userWarningList}>
              {plan.blockingReasons.map((reason) => <li key={reason}>{reason}</li>)}
            </ul>
          </StatusMessage>
        ) : null}
        <div className={styles.userPlanChanges}>
          {plan?.changes.map((change) => (
            <section
              key={`${change.target}-${change.path}-${change.operation}`}
              className={styles.userPlanChange}
              data-testid={`user-instruction-plan-${change.target}`}
            >
              <div className={styles.userPlanHeader}>
                <div>
                  <h3>{t(`agentHub:targets.${change.target}`)}</h3>
                  <code className={styles.userPath}>{change.path}</code>
                </div>
                <Pill tone={change.operation === 'leave' ? 'neutral' : 'warn'}>
                  {t(`agentHub:userInstructions.plan.operations.${change.operation}`)}
                </Pill>
              </div>
              {change.willShadowSourcePath ? (
                <StatusMessage tone="warn" live="off">
                  {t('agentHub:userInstructions.plan.willShadow', {
                    path: change.willShadowSourcePath,
                  })}
                </StatusMessage>
              ) : null}
              {change.willReplaceFallbackSourcePath ? (
                <StatusMessage tone="warn" live="off">
                  {t('agentHub:userInstructions.plan.willReplaceFallback', {
                    path: change.willReplaceFallbackSourcePath,
                  })}
                </StatusMessage>
              ) : null}
              {change.emptyDueToTargetOnly ? (
                <StatusMessage tone="warn" live="off">
                  {t('agentHub:userInstructions.plan.emptyTargetBlocked')}
                </StatusMessage>
              ) : null}
              <p className={styles.userPlanActivation}>
                {t('agentHub:userInstructions.plan.activation', {
                  mode: t(`agentHub:userInstructions.plan.activationModes.${change.activation}`),
                })}
              </p>
              {change.unifiedDiff ? (
                <div className={styles.userDiffBlock} tabIndex={0}>
                  <pre>{change.unifiedDiff}</pre>
                  {change.diffTruncated ? (
                    <p>{t('agentHub:userInstructions.plan.diffTruncated')}</p>
                  ) : null}
                </div>
              ) : (
                <p className={styles.userPathEmpty}>
                  {t('agentHub:userInstructions.plan.noDiff')}
                </p>
              )}
              {change.warnings.length ? (
                <ul className={styles.userWarningList}>
                  {change.warnings.map((warning) => <li key={warning}>{warning}</li>)}
                </ul>
              ) : null}
            </section>
          ))}
        </div>
        <footer className={styles.userDialogActions}>
          <Button variant="ghost" size="sm" disabled={busy} onClick={onClose}>
            {t('common:action.cancel')}
          </Button>
          <Button
            variant="primary"
            size="sm"
            loading={busy}
            disabled={!plan || blocked || Boolean(plan.truncated)}
            onClick={onApply}
            data-testid="user-instruction-apply-plan"
          >
            {t('agentHub:userInstructions.plan.apply', { count: actionableCount })}
          </Button>
        </footer>
      </div>
    </Dialog>
  );
}
