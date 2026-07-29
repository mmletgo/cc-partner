/**
 * AssetAdoptionDialog — externalCollision / adoption 预览 pure 视图。
 *
 * Business Logic（为什么需要这个对话框）:
 *   externalCollision 必须先展示来源与诊断；LAN push 在 Gate C，
 *   本轮不宣称可跨设备推送或静默移动 Skill 目录。
 *
 * Code Logic（这个组件做什么）:
 *   复用 Dialog；纯 props；禁止 import @/api/*。
 */

import { useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Dialog, StatusMessage } from '@/components/primitives';
import type { AgentHubAdoptionPreview } from '@/lib/types/agentHub';
import styles from './AgentHub.module.css';

export interface AssetAdoptionDialogProps {
  open: boolean;
  preview: AgentHubAdoptionPreview | null;
  busy?: boolean;
  onClose: () => void;
  onConfirm?: () => void;
}

/**
 * Business Logic: 碰撞/纳管确认对话框。
 * Code Logic: hooks 在 early return 前。
 */
export function AssetAdoptionDialog({
  open,
  preview,
  busy = false,
  onClose,
  onConfirm,
}: AssetAdoptionDialogProps) {
  const { t } = useTranslation(['agentHub', 'common']);
  const focusRef = useRef<HTMLButtonElement | null>(null);

  return (
    <Dialog
      open={open}
      titleId="agent-hub-adoption-title"
      onClose={onClose}
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
      initialFocusRef={focusRef}
      className={styles.dialogSurface}
    >
      <div className={styles.dialogBody} data-testid="agent-hub-adoption-dialog">
        <h2 id="agent-hub-adoption-title" className={styles.drawerTitle}>
          {t('agentHub:adoption.title')}
        </h2>
        <p className={styles.drawerSubtitle}>{t('agentHub:adoption.desc')}</p>
        <StatusMessage tone="info" live="off" data-testid="agent-hub-lan-push-gate-c">
          {t('agentHub:adoption.lanPushGateC')}
        </StatusMessage>
        {preview ? (
          <div className={styles.previewResult} data-testid="agent-hub-adoption-preview">
            <div className={styles.metaBlock}>
              <div>
                <span className={styles.metaLabel}>{t('agentHub:matrix.canonical')}</span>
                <span data-testid="adoption-canonical">{preview.displayName}</span>
              </div>
              <div>
                <span className={styles.metaLabel}>{t('agentHub:adoption.logicalKey')}</span>
                <span className={styles.mono}>{preview.logicalKey}</span>
              </div>
              <div>
                <span className={styles.metaLabel}>{t('agentHub:adoption.origin')}</span>
                <span>{preview.originNamespace}</span>
              </div>
              <div>
                <span className={styles.metaLabel}>{t('agentHub:adoption.target')}</span>
                <span>{t(`agentHub:targets.${preview.target}`)}</span>
              </div>
              <div>
                <span className={styles.metaLabel}>{t('agentHub:matrix.aggregate')}</span>
                <span>{t(`agentHub:aggregate.${preview.aggregateStatus}`)}</span>
              </div>
            </div>
            {preview.diagnostics.length > 0 ? (
              <ul className={styles.warningList} data-testid="adoption-diagnostics">
                {preview.diagnostics.map((line) => (
                  <li key={line}>{line}</li>
                ))}
              </ul>
            ) : (
              <p className={styles.hint}>{t('agentHub:adoption.noDiagnostics')}</p>
            )}
          </div>
        ) : (
          <p className={styles.emptyInline}>{t('agentHub:adoption.empty')}</p>
        )}
        <div className={styles.dialogActions}>
          <Button
            ref={focusRef}
            variant="secondary"
            size="sm"
            disabled={busy}
            onClick={onClose}
            data-testid="agent-hub-adoption-close"
          >
            {t('common:action.cancel')}
          </Button>
          {onConfirm ? (
            <Button
              variant="primary"
              size="sm"
              loading={busy}
              disabled={!preview}
              onClick={onConfirm}
              data-testid="agent-hub-adoption-confirm"
            >
              {t('agentHub:adoption.confirm')}
            </Button>
          ) : null}
        </div>
      </div>
    </Dialog>
  );
}
