import type { JSX } from 'react';
import { Button, Card, Pill, StatusMessage } from '@/components/primitives';
import type { UserInstructionTargetDto } from '@/lib/types/agentHub';
import type { TFunction } from 'i18next';
import {
  getUserInstructionTargetPresentation,
  type UserInstructionTargetPresentation,
} from './userInstructionPresentation';
import type { UserInstructionTargetIntent } from './useUserInstructionManager';
import styles from '../AgentHub.module.css';

export interface UserInstructionTargetCardProps {
  t: TFunction<['agentHub', 'common']>;
  target: UserInstructionTargetDto;
  busy: boolean;
  onIntent: (target: UserInstructionTargetDto, intent: UserInstructionTargetIntent) => void;
  onOpenPath: (path: string) => void;
  onCopyPath: (path: string) => void;
}

/** 返回动作是否由后端安全矩阵允许。 */
function hasAction(target: UserInstructionTargetDto, action: string): boolean {
  return target.availableActions.includes(action as never);
}

/** 渲染一个 target 的可用业务动作，不从 capability 自行发明危险动作。 */
function TargetActions(props: {
  t: TFunction<['agentHub', 'common']>;
  target: UserInstructionTargetDto;
  presentation: UserInstructionTargetPresentation;
  busy: boolean;
  onIntent: (target: UserInstructionTargetDto, intent: UserInstructionTargetIntent) => void;
}): JSX.Element | null {
  const { t, target, presentation, busy, onIntent } = props;
  const actions: Array<{ action: UserInstructionTargetIntent; variant: 'primary' | 'secondary' | 'ghost' }> = [];
  if (hasAction(target, 'manage')) actions.push({ action: 'manage', variant: 'primary' });
  if (hasAction(target, 'resume')) actions.push({ action: 'resume', variant: 'primary' });
  if (hasAction(target, 'restore')) actions.push({ action: 'restore', variant: 'primary' });
  if (hasAction(target, 'compare')) actions.push({ action: 'compare', variant: 'secondary' });
  if (hasAction(target, 'adopt')) actions.push({ action: 'adopt', variant: 'secondary' });
  if (hasAction(target, 'pause')) actions.push({ action: 'pause', variant: 'secondary' });
  if (hasAction(target, 'stopManaging')) actions.push({ action: 'stopManaging', variant: 'ghost' });
  if (hasAction(target, 'remove')) actions.push({ action: 'remove', variant: 'ghost' });
  if (actions.length === 0 && presentation.capabilityKey !== 'automatic') return null;
  return (
    <div className={styles.userTargetActions}>
      {actions.map(({ action, variant }) => (
        <Button
          key={action}
          variant={variant}
          size="sm"
          disabled={busy}
          onClick={() => onIntent(target, action)}
          data-testid={`user-instruction-${target.target}-${action}`}
        >
          {t(`agentHub:userInstructions.actions.${action}`)}
        </Button>
      ))}
    </div>
  );
}

/**
 * Business Logic（为什么需要）:
 *   target 卡必须先回答 CLI 是否存在、实际读哪个文件、Hub 管哪个路径和下一步。
 *
 * Code Logic（做什么）:
 *   只渲染一个主状态 Pill 与一个能力 StatusMessage，并复用后端 availableActions。
 */
export function UserInstructionTargetCard(props: UserInstructionTargetCardProps): JSX.Element {
  const { t, target, busy, onIntent, onOpenPath, onCopyPath } = props;
  const presentation = getUserInstructionTargetPresentation(target);
  const sourcePath = presentation.activeSource?.path ?? '';
  const managedPath = target.managedTargetPath;
  return (
    <Card
      variant="outlined"
      padding="none"
      className={styles.userTargetCard}
      data-testid={`user-instruction-target-${target.target}`}
    >
      <Card.Header padding="md" className={styles.userTargetHeader}>
        <div>
          <h3 className={styles.userTargetName}>{t(`agentHub:targets.${target.target}`)}</h3>
          <p className={styles.userTargetCli}>
            {target.cli.installed
              ? t('agentHub:userInstructions.target.cliInstalled', {
                  version: target.cli.version ?? t('agentHub:probes.unknownVersion'),
                })
              : t('agentHub:userInstructions.target.cliNotInstalled')}
          </p>
        </div>
        <Pill tone={presentation.tone}>{t(`agentHub:userInstructions.target.states.${presentation.stateKey}`)}</Pill>
      </Card.Header>
      <Card.Body padding="md" className={styles.userTargetBody}>
        <div className={styles.userPathGroup}>
          <span className={styles.userPathLabel}>
            {t('agentHub:userInstructions.target.effectiveSource')}
          </span>
          {sourcePath ? (
            <div className={styles.userPathRow}>
              <code className={styles.userPath}>{sourcePath}</code>
              <div className={styles.userPathActions}>
                <Button variant="ghost" size="sm" onClick={() => onCopyPath(sourcePath)}>
                  {t('common:action.copy')}
                </Button>
                <Button variant="ghost" size="sm" onClick={() => onOpenPath(sourcePath)}>
                  {t('agentHub:userInstructions.actions.openFile')}
                </Button>
              </div>
            </div>
          ) : (
            <span className={styles.userPathEmpty}>
              {target.capability.reasonCode === 'USER_INSTRUCTION_V2_BACKEND_UNAVAILABLE'
                ? t('agentHub:userInstructions.target.sourceResolverUnavailable')
                : t('agentHub:userInstructions.target.noEffectiveSource')}
            </span>
          )}
        </div>
        {presentation.shadowedSources.length > 0 ? (
          <div className={styles.userPathGroup} data-testid={`user-instruction-shadowed-${target.target}`}>
            <span className={styles.userPathLabel}>
              {t('agentHub:userInstructions.target.shadowedSources')}
            </span>
            {presentation.shadowedSources.map((source) => (
              <code key={source.sourceId} className={styles.userPath}>{source.path}</code>
            ))}
          </div>
        ) : null}
        {managedPath ? (
          <div className={styles.userPathGroup}>
            <span className={styles.userPathLabel}>
              {t('agentHub:userInstructions.target.managedPath')}
            </span>
            <code className={styles.userPath}>{managedPath}</code>
          </div>
        ) : null}
        <StatusMessage tone={presentation.capabilityKey === 'scanBlocked' ? 'warn' : 'info'} live="off">
          {t(`agentHub:userInstructions.target.capabilities.${presentation.capabilityKey}`)}
        </StatusMessage>
        {target.projection.lastErrorCode ? (
          <p className={styles.userTargetReason}>
            {t('agentHub:userInstructions.target.reasonCode', { code: target.projection.lastErrorCode })}
          </p>
        ) : null}
      </Card.Body>
      <Card.Footer padding="md" className={styles.userTargetFooter}>
        <TargetActions
          t={t}
          target={target}
          presentation={presentation}
          busy={busy}
          onIntent={onIntent}
        />
      </Card.Footer>
    </Card>
  );
}
