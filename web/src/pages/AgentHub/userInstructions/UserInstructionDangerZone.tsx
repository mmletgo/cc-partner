import type { JSX } from 'react';
import { Button, Card, Dialog, Input, StatusMessage } from '@/components/primitives';
import type { TFunction } from 'i18next';
import styles from '../AgentHub.module.css';

export interface UserInstructionDangerZoneProps {
  t: TFunction<['agentHub', 'common']>;
  displayName: string;
  open: boolean;
  confirmation: string;
  busy: boolean;
  onOpen: () => void;
  onClose: () => void;
  onConfirmationChange: (value: string) => void;
  onPreviewDelete: () => void;
}

/**
 * Business Logic（为什么需要）:
 *   canonical 全量删除与 target-local 动作必须分离，并在危险区要求名称确认和路径预览。
 *
 * Code Logic（做什么）:
 *   Card 只提供次级入口；Dialog 校验显示名后进入统一 plan preview，不直接删除。
 */
export function UserInstructionDangerZone(props: UserInstructionDangerZoneProps): JSX.Element {
  const {
    t,
    displayName,
    open,
    confirmation,
    busy,
    onOpen,
    onClose,
    onConfirmationChange,
    onPreviewDelete,
  } = props;
  return (
    <>
      <Card variant="outlined" padding="md" className={styles.userDangerZone}>
        <Card.Header>
          <div>
            <h2 className={styles.userSectionTitle}>{t('agentHub:userInstructions.danger.title')}</h2>
            <p className={styles.userSectionDescription}>
              {t('agentHub:userInstructions.danger.description')}
            </p>
          </div>
        </Card.Header>
        <Card.Footer>
          <Button variant="danger" size="sm" onClick={onOpen}>
            {t('agentHub:userInstructions.danger.open')}
          </Button>
        </Card.Footer>
      </Card>
      <Dialog
        open={open}
        titleId="user-instruction-delete-title"
        onClose={onClose}
        closeOnEscape={!busy}
        closeOnBackdrop={!busy}
        className={styles.userDialogSurface}
      >
        <div className={styles.userDialogBody} data-testid="user-instruction-delete-dialog">
          <h2 id="user-instruction-delete-title" className={styles.userDialogTitle}>
            {t('agentHub:userInstructions.danger.dialogTitle')}
          </h2>
          <StatusMessage tone="danger">
            {t('agentHub:userInstructions.danger.warning')}
          </StatusMessage>
          <label className={styles.userDeleteField}>
            <span>{t('agentHub:userInstructions.danger.confirmLabel', { name: displayName })}</span>
            <Input
              value={confirmation}
              onChange={(event) => onConfirmationChange(event.currentTarget.value)}
              data-testid="user-instruction-delete-confirmation"
            />
          </label>
          <div className={styles.userDialogActions}>
            <Button variant="ghost" size="sm" disabled={busy} onClick={onClose}>
              {t('common:action.cancel')}
            </Button>
            <Button
              variant="danger"
              size="sm"
              loading={busy}
              disabled={confirmation !== displayName}
              onClick={onPreviewDelete}
              data-testid="user-instruction-delete-preview"
            >
              {t('agentHub:userInstructions.danger.preview')}
            </Button>
          </div>
        </div>
      </Dialog>
    </>
  );
}
