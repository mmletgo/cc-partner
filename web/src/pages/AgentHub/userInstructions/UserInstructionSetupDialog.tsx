import { useRef, type JSX } from 'react';
import { Button, Dialog, Pill, StatusMessage } from '@/components/primitives';
import type {
  AgentTarget,
  UserInstructionDraft,
  UserInstructionTargetDto,
  UserInstructionTargetSelection,
} from '@/lib/types/agentHub';
import type { TFunction } from 'i18next';
import styles from '../AgentHub.module.css';

export interface UserInstructionSetupDialogProps {
  t: TFunction<['agentHub', 'common']>;
  open: boolean;
  busy: boolean;
  targets: UserInstructionTargetDto[];
  draft: UserInstructionDraft;
  canPreview: boolean;
  error: string | null;
  onClose: () => void;
  onSelectionChange: (
    target: AgentTarget,
    selection: UserInstructionTargetSelection,
  ) => void;
  onPromoteToCommon: (target: AgentTarget) => void;
  onPreview: () => void;
}

/**
 * Business Logic（为什么需要）:
 *   首次设置必须显式选择公共/专属表达和每个 Agent 的管理意图，确认前保持零 mutation。
 *
 * Code Logic（做什么）:
 *   共享 Dialog 承载原生 radio 语义；scan-only target 禁止选择 managed，但仍可保持现状或 fallback。
 */
export function UserInstructionSetupDialog(props: UserInstructionSetupDialogProps): JSX.Element {
  const {
    t,
    open,
    busy,
    targets,
    draft,
    canPreview,
    error,
    onClose,
    onSelectionChange,
    onPromoteToCommon,
    onPreview,
  } = props;
  const firstRadioRef = useRef<HTMLInputElement | null>(null);
  const promotableTarget = targets.find(
    (target) =>
      Boolean(draft.targetExtensions[target.target]?.trim()) && !draft.commonContent.trim(),
  );
  return (
    <Dialog
      open={open}
      titleId="user-instruction-setup-title"
      onClose={onClose}
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
      initialFocusRef={firstRadioRef}
      className={styles.userDialogSurface}
    >
      <div className={styles.userDialogBody} data-testid="user-instruction-setup-dialog">
        <header className={styles.userDialogHeader}>
          <h2 id="user-instruction-setup-title" className={styles.userDialogTitle}>
            {t('agentHub:userInstructions.setup.title')}
          </h2>
          <p className={styles.userSectionDescription}>
            {t('agentHub:userInstructions.setup.description')}
          </p>
        </header>
        {promotableTarget ? (
          <div className={styles.userSetupSourceChoice}>
            <div>
              <strong>{t('agentHub:userInstructions.setup.singleSourceTitle', {
                target: t(`agentHub:targets.${promotableTarget.target}`),
              })}</strong>
              <p className={styles.userSectionDescription}>
                {t('agentHub:userInstructions.setup.singleSourceDescription')}
              </p>
            </div>
            <Button
              variant="secondary"
              size="sm"
              onClick={() => onPromoteToCommon(promotableTarget.target)}
            >
              {t('agentHub:userInstructions.setup.useAsCommon')}
            </Button>
          </div>
        ) : null}
        {error ? <StatusMessage tone="danger">{error}</StatusMessage> : null}
        <div className={styles.userSetupTargets}>
          {targets.map((target, targetIndex) => {
            const activeSource = target.sources.find((source) => source.active);
            const canManage = target.capability.write === 'supported';
            const canInherit = activeSource?.role === 'fallback';
            return (
              <fieldset key={target.target} className={styles.userSetupTarget}>
                <legend>
                  <span>{t(`agentHub:targets.${target.target}`)}</span>
                  <Pill tone={canManage ? 'success' : 'neutral'}>
                    {canManage
                      ? t('agentHub:userInstructions.setup.writable')
                      : t('agentHub:userInstructions.setup.scanOnly')}
                  </Pill>
                </legend>
                <code className={styles.userPath}>
                  {target.managedTargetPath || t('agentHub:userInstructions.target.pathUnavailable')}
                </code>
                <label className={styles.userRadioRow}>
                  <input
                    ref={targetIndex === 0 ? firstRadioRef : undefined}
                    type="radio"
                    name={`user-instruction-target-${target.target}`}
                    value="managed"
                    checked={draft.targetSelections[target.target] === 'managed'}
                    disabled={!canManage || busy}
                    onChange={() => onSelectionChange(target.target, 'managed')}
                  />
                  <span>{t('agentHub:userInstructions.setup.manage')}</span>
                </label>
                <label className={styles.userRadioRow}>
                  <input
                    type="radio"
                    name={`user-instruction-target-${target.target}`}
                    value="unmanaged"
                    checked={draft.targetSelections[target.target] === 'unmanaged'}
                    disabled={busy}
                    onChange={() => onSelectionChange(target.target, 'unmanaged')}
                  />
                  <span>{t('agentHub:userInstructions.setup.leave')}</span>
                </label>
                {canInherit ? (
                  <label className={styles.userRadioRow}>
                    <input
                      type="radio"
                      name={`user-instruction-target-${target.target}`}
                      value="inherit"
                      checked={draft.targetSelections[target.target] === 'inherit'}
                      disabled={busy}
                      onChange={() => onSelectionChange(target.target, 'inherit')}
                    />
                    <span>{t('agentHub:userInstructions.setup.inherit')}</span>
                  </label>
                ) : null}
                {!canManage ? (
                  <p className={styles.userSetupReason}>
                    {t('agentHub:userInstructions.setup.scanOnlyReason')}
                  </p>
                ) : null}
              </fieldset>
            );
          })}
        </div>
        <footer className={styles.userDialogActions}>
          <Button variant="ghost" size="sm" disabled={busy} onClick={onClose}>
            {t('common:action.cancel')}
          </Button>
          <Button
            variant="primary"
            size="sm"
            loading={busy}
            disabled={!canPreview}
            onClick={onPreview}
            data-testid="user-instruction-setup-preview"
          >
            {t('agentHub:userInstructions.setup.preview')}
          </Button>
        </footer>
      </div>
    </Dialog>
  );
}
