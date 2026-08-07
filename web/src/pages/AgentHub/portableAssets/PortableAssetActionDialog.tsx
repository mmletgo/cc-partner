/**
 * PortableAssetActionDialog — 本机 portable 资产 preview/apply 确认 Dialog。
 *
 * Business Logic（为什么需要这个对话框）:
 *   所有本机 mutation 必须 inspect→preview→confirm→apply；partial/outcomeUnknown 诚实；
 *   危险动作不用 window.confirm。
 *
 * Code Logic（这个组件做什么）:
 *   pure props：回调由 controller 持有 planToken/clientRequestId；views 不 import @/api/*。
 */

import { useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Dialog, Pill, StatusMessage } from '@/components/primitives';
import type {
  PortableAssetActionKind,
  PortableAssetActionPlanDto,
  PortableAssetActionResultDto,
  PortableInventoryItemDto,
  PreviewPortableAssetActionRequest,
} from '@/lib/types/portableInventory';
import styles from '../AgentHub.module.css';

export interface PortableAssetActionDialogProps {
  open: boolean;
  item: PortableInventoryItemDto | null;
  action: PortableAssetActionKind | null;
  inventorySnapshotHash: string | null;
  plan: PortableAssetActionPlanDto | null;
  result: PortableAssetActionResultDto | null;
  busy: boolean;
  error: string | null;
  clientRequestId: string | null;
  onPreview: (request: PreviewPortableAssetActionRequest) => void;
  onConfirm: (planToken: string, clientRequestId: string) => void;
  onReconcile: (clientRequestId: string) => void;
  onClose: () => void;
}

/**
 * Business Logic: 聚合结果 outcome 不得把 partial/unknown 压成全成功。
 * Code Logic: 扫描 item states。
 */
export function classifyActionOutcome(
  result: PortableAssetActionResultDto | null,
): 'none' | 'fullSuccess' | 'partial' | 'outcomeUnknown' | 'failed' {
  if (!result || result.items.length === 0) return 'none';
  const states = result.items.map((row) => row.state);
  if (states.some((s) => s === 'outcomeUnknown')) return 'outcomeUnknown';
  if (states.every((s) => s === 'succeeded' || s === 'skipped')) return 'fullSuccess';
  if (states.some((s) => s === 'succeeded' || s === 'skipped') && states.some((s) => s === 'failed' || s === 'blocked')) {
    return 'partial';
  }
  if (states.every((s) => s === 'failed' || s === 'blocked')) return 'failed';
  return 'partial';
}

/**
 * Business Logic: plan 是否允许 confirm。
 * Code Logic: 无 blocking + 至少一条无 block change。
 */
function planIsActionable(plan: PortableAssetActionPlanDto | null): boolean {
  if (!plan) return false;
  if (plan.blockingReasons.length > 0) return false;
  return plan.changes.some((change) => change.blockingReasons.length === 0);
}

/**
 * Business Logic: preview/apply 共享 Dialog。
 * Code Logic: hooks 在 early return 前；busy 禁用 close。
 */
export function PortableAssetActionDialog({
  open,
  item,
  action,
  inventorySnapshotHash,
  plan,
  result,
  busy,
  error,
  clientRequestId,
  onPreview,
  onConfirm,
  onReconcile,
  onClose,
}: PortableAssetActionDialogProps) {
  const { t } = useTranslation(['agentHub', 'common']);
  const focusRef = useRef<HTMLButtonElement | null>(null);
  const [keepData, setKeepData] = useState(false);
  const [conflictPolicy, setConflictPolicy] = useState<'skipExisting' | 'replaceAfterPreview'>(
    'skipExisting',
  );

  const outcome = useMemo(() => classifyActionOutcome(result), [result]);
  const canConfirm = Boolean(
    plan &&
      clientRequestId &&
      planIsActionable(plan) &&
      !busy &&
      outcome === 'none',
  );
  const canPreview = Boolean(item && action && inventorySnapshotHash && !busy);

  /**
   * Business Logic: preview 必须显式发送 keepData/conflictPolicy/expected revision。
   * Code Logic: 组装 PreviewPortableAssetActionRequest。
   */
  function handlePreview() {
    if (!item || !action || !inventorySnapshotHash) return;
    onPreview({
      inventorySnapshotHash,
      inventoryItemIds: [item.inventoryItemId],
      action,
      keepData,
      conflictPolicy,
      expectedCanonicalRevisionId: item.canonicalRevisionId,
    });
  }

  /**
   * Business Logic: confirm 只透传 planToken + clientRequestId。
   * Code Logic: no window.confirm。
   */
  function handleConfirm() {
    if (!plan || !clientRequestId || !canConfirm) return;
    onConfirm(plan.planToken, clientRequestId);
  }

  /**
   * Business Logic: busy 时禁止关闭。
   * Code Logic: guarded onClose。
   */
  function handleClose() {
    if (busy) return;
    onClose();
  }

  return (
    <Dialog
      open={open}
      titleId="portable-asset-action-title"
      onClose={handleClose}
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
      initialFocusRef={focusRef}
      className={styles.dialogSurface}
    >
      <div className={styles.dialogBody} data-testid="portable-asset-action-dialog">
        <h2 id="portable-asset-action-title" className={styles.drawerTitle}>
          {t('agentHub:portable.actionDialog.title', {
            action: action ?? 'unknown',
            name: item?.displayName ?? '',
          })}
        </h2>
        <p className={styles.drawerSubtitle}>{t('agentHub:portable.actionDialog.subtitle')}</p>

        {error ? (
          <StatusMessage tone="danger" data-testid="portable-action-error">
            {error}
          </StatusMessage>
        ) : null}

        {item ? (
          <div className={styles.metaBlock} data-testid="portable-action-item-summary">
            <div>
              <span className={styles.metaLabel}>{t('agentHub:portable.details.nativeId')}</span>
              <span className={styles.mono}>{item.nativeId}</span>
            </div>
            <div>
              <span className={styles.metaLabel}>{t('agentHub:kinds.' + item.kind)}</span>
              <span>{t(`agentHub:targets.${item.target}`)}</span>
            </div>
          </div>
        ) : null}

        {action === 'uninstall' ? (
          <label className={styles.hint} data-testid="portable-action-keep-data-label">
            <input
              type="checkbox"
              checked={keepData}
              disabled={busy || Boolean(plan)}
              onChange={(event) => setKeepData(event.target.checked)}
              data-testid="portable-action-keep-data"
            />{' '}
            {t('agentHub:portable.actionDialog.keepData')}
          </label>
        ) : null}

        {action === 'installToSourceTarget' || action === 'adopt' ? (
          <label className={styles.hint} data-testid="portable-action-conflict-policy-label">
            {t('agentHub:portable.actionDialog.conflictPolicy')}
            <select
              value={conflictPolicy}
              disabled={busy || Boolean(plan)}
              onChange={(event) =>
                setConflictPolicy(event.target.value as 'skipExisting' | 'replaceAfterPreview')
              }
              data-testid="portable-action-conflict-policy"
            >
              <option value="skipExisting">
                {t('agentHub:portable.actionDialog.conflictSkip')}
              </option>
              <option value="replaceAfterPreview">
                {t('agentHub:portable.actionDialog.conflictReplace')}
              </option>
            </select>
          </label>
        ) : null}

        {plan ? (
          <section className={styles.drawerSection} data-testid="portable-action-plan">
            <div className={styles.metaBlock}>
              <div>
                <span className={styles.metaLabel}>
                  {t('agentHub:portable.actionDialog.planToken')}
                </span>
                <span data-testid="portable-action-plan-token" className={styles.mono}>
                  {plan.planToken}
                </span>
              </div>
              <div>
                <span className={styles.metaLabel}>
                  {t('agentHub:portable.actionDialog.keepData')}
                </span>
                <span data-testid="portable-action-plan-keep-data">
                  {plan.keepData ? 'true' : 'false'}
                </span>
              </div>
            </div>
            {plan.blockingReasons.length > 0 ? (
              <StatusMessage tone="warn" data-testid="portable-action-blocking">
                <ul className={styles.partialList}>
                  {plan.blockingReasons.map((reason) => (
                    <li key={reason}>{reason}</li>
                  ))}
                </ul>
              </StatusMessage>
            ) : null}
            <ul className={styles.partialList} data-testid="portable-action-changes">
              {plan.changes.map((change) => (
                <li
                  key={`${change.inventoryItemId}-${change.operation}`}
                  data-testid={`portable-action-change-${change.inventoryItemId}`}
                  data-operation={change.operation}
                >
                  <Pill tone={change.blockingReasons.length ? 'danger' : 'warn'}>
                    {change.operation}
                  </Pill>{' '}
                  <span className={styles.mono}>{change.path ?? change.inventoryItemId}</span>
                  {change.blockingReasons.length > 0
                    ? ` · ${change.blockingReasons.join(', ')}`
                    : ''}
                  {change.warnings.length > 0 ? ` · ${change.warnings.join(', ')}` : ''}
                </li>
              ))}
            </ul>
          </section>
        ) : null}

        {result ? (
          <section
            className={styles.drawerSection}
            data-testid="portable-action-result"
            data-outcome={outcome}
          >
            {outcome === 'fullSuccess' ? (
              <StatusMessage tone="success" data-testid="portable-action-full-success">
                {t('agentHub:portable.actionDialog.fullSuccess')}
              </StatusMessage>
            ) : null}
            {outcome === 'partial' ? (
              <StatusMessage tone="warn" data-testid="portable-action-partial">
                {t('agentHub:portable.actionDialog.partial')}
              </StatusMessage>
            ) : null}
            {outcome === 'outcomeUnknown' ? (
              <StatusMessage tone="warn" data-testid="portable-action-outcome-unknown">
                {t('agentHub:portable.actionDialog.outcomeUnknown')}
              </StatusMessage>
            ) : null}
            <ul className={styles.partialList}>
              {result.items.map((row) => (
                <li
                  key={row.inventoryItemId}
                  data-testid={`portable-action-item-${row.inventoryItemId}`}
                  data-state={row.state}
                >
                  <Pill
                    tone={
                      row.state === 'succeeded'
                        ? 'success'
                        : row.state === 'skipped'
                          ? 'neutral'
                          : 'danger'
                    }
                  >
                    {row.state}
                  </Pill>{' '}
                  <span className={styles.mono}>{row.inventoryItemId}</span>
                  {row.errorCode ? ` · ${row.errorCode}` : ''}
                  {row.message ? ` · ${row.message}` : ''}
                </li>
              ))}
            </ul>
            {outcome === 'outcomeUnknown' && clientRequestId ? (
              <Button
                variant="secondary"
                size="sm"
                disabled={busy}
                onClick={() => onReconcile(clientRequestId)}
                data-testid="portable-action-reconcile"
              >
                {t('agentHub:portable.actionDialog.reconcile')}
              </Button>
            ) : null}
          </section>
        ) : null}

        <div className={styles.dialogActions}>
          <Button
            ref={focusRef}
            variant="secondary"
            size="sm"
            disabled={busy}
            onClick={handleClose}
            data-testid="portable-action-close"
          >
            {t('common:action.cancel')}
          </Button>
          {!plan ? (
            <Button
              variant="primary"
              size="sm"
              loading={busy}
              disabled={!canPreview}
              onClick={handlePreview}
              data-testid="portable-action-run-preview"
            >
              {t('agentHub:portable.actionDialog.preview')}
            </Button>
          ) : (
            <Button
              variant={action === 'uninstall' ? 'danger' : 'primary'}
              size="sm"
              loading={busy}
              disabled={!canConfirm}
              onClick={handleConfirm}
              data-testid="portable-action-confirm"
            >
              {t('agentHub:portable.actionDialog.confirm')}
            </Button>
          )}
        </div>
      </div>
    </Dialog>
  );
}

PortableAssetActionDialog.displayName = 'PortableAssetActionDialog';
