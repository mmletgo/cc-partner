/**
 * PortableAssetActionDialog — 本机 portable 资产确认 Dialog。
 *
 * Business Logic（为什么需要这个对话框）:
 *   所有本机 mutation 仍须 inspect→preview→confirm→apply；用户只点一次确认。
 *   打开即自动 preview，危险动作不用 window.confirm，也不再点「预览」按钮。
 *
 * Code Logic（这个组件做什么）:
 *   pure props：回调由 controller 持有 planToken/clientRequestId；views 不 import @/api/*。
 *   keepData / conflictPolicy 变化会自动重跑 preview，避免确认过期计划。
 */

import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Dialog, Pill, StatusMessage } from '@/components/primitives';
import { isHubTarget } from '@/lib/agentCatalog';
import type {
  PortableAssetActionKind,
  PortableAssetActionPlanDto,
  PortableAssetActionResultDto,
  PortableInventoryItemDto,
  PreviewPortableAssetActionRequest,
} from '@/lib/types/portableInventory';
import {
  isPortableBorrowedRuntimeItem,
  portableBorrowedOwnerLabelKey,
} from './portableInventoryPresentation';
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
  /** inventory stale / mutationBlocked 时禁止 preview/confirm。 */
  mutationBlocked?: boolean;
  stale?: boolean;
  onPreview: (request: PreviewPortableAssetActionRequest) => void;
  onConfirm: (planToken: string, clientRequestId: string) => void;
  onReconcile: (clientRequestId: string) => void;
  onClose: () => void;
}

/**
 * Business Logic: 聚合结果 outcome 不得把 partial/unknown 压成全成功。
 * Code Logic: 扫描 item states。
 */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located for dialog consumers/tests
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
 * Business Logic: 借用 skill/command/MCP 启停会改所有者磁盘；plugin 启停只改当前 Agent 开关。
 * Code Logic: plugin 的 enable/disable 用独立文案；卸载仍走所有者影响提示。
 */
type BorrowedImpactKey =
  | 'borrowedImpactEnable'
  | 'borrowedImpactDisable'
  | 'borrowedImpactEnablePlugin'
  | 'borrowedImpactDisablePlugin'
  | 'borrowedImpactUninstall';

function borrowedImpactKey(
  action: PortableAssetActionKind | null,
  kind: PortableInventoryItemDto['kind'] | undefined,
): BorrowedImpactKey | null {
  const plugin = kind === 'plugin';
  if (action === 'enable') return plugin ? 'borrowedImpactEnablePlugin' : 'borrowedImpactEnable';
  if (action === 'disable') return plugin ? 'borrowedImpactDisablePlugin' : 'borrowedImpactDisable';
  if (action === 'uninstall') return 'borrowedImpactUninstall';
  return null;
}

/**
 * Business Logic: Skill/Command/MCP 的仓库动作文案不得复用 Plugin viewing-switch。
 * Code Logic: attach/detach/migrate/destroy 各用独立 hint；Plugin 保持原开关语义。
 */
function storeActionHintKey(
  action: PortableAssetActionKind | null,
  kind: PortableInventoryItemDto['kind'] | undefined,
):
  | 'storeAttachHint'
  | 'storeDetachHint'
  | 'storeMigrateHint'
  | 'storeDestroyHint'
  | null {
  if (kind === 'plugin') return null;
  if (action === 'attach') return 'storeAttachHint';
  if (action === 'detach') return 'storeDetachHint';
  if (action === 'migrateToStore') return 'storeMigrateHint';
  if (action === 'destroyStore') return 'storeDestroyHint';
  return null;
}

/**
 * Business Logic: 打开即自动 preview，用户只确认或取消。
 * Code Logic: hooks 在 early return 前；busy 禁用 close；选项变化自动重跑 preview。
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
  mutationBlocked = false,
  stale = false,
  onPreview,
  onConfirm,
  onReconcile,
  onClose,
}: PortableAssetActionDialogProps) {
  const { t } = useTranslation(['agentHub', 'common']);
  const focusRef = useRef<HTMLButtonElement | null>(null);
  const previewedKeyRef = useRef<string | null>(null);
  const sessionKey = `${open ? 'open' : 'closed'}|${item?.inventoryItemId ?? ''}|${action ?? ''}`;
  const [options, setOptions] = useState<{
    sessionKey: string;
    keepData: boolean;
    conflictPolicy: 'skipExisting' | 'replaceAfterPreview';
  }>({
    sessionKey,
    keepData: false,
    conflictPolicy: 'skipExisting',
  });
  if (options.sessionKey !== sessionKey) {
    setOptions({
      sessionKey,
      keepData: false,
      conflictPolicy: 'skipExisting',
    });
  }
  const keepData = options.sessionKey === sessionKey ? options.keepData : false;
  const conflictPolicy =
    options.sessionKey === sessionKey ? options.conflictPolicy : 'skipExisting';

  const outcome = useMemo(() => classifyActionOutcome(result), [result]);
  // Global Constraints: stale 禁止 mutation —— preview 后 inventory 变 stale 也不得 confirm。
  const inventoryBlocked = mutationBlocked || stale;
  const planMatchesOptions = Boolean(
    plan &&
      plan.action === action &&
      plan.keepData === keepData &&
      plan.conflictPolicy === conflictPolicy &&
      plan.inventorySnapshotHash === inventorySnapshotHash,
  );
  const canConfirm = Boolean(
    plan &&
      planMatchesOptions &&
      clientRequestId &&
      planIsActionable(plan) &&
      !busy &&
      !inventoryBlocked &&
      outcome === 'none',
  );
  const canAutoPreview = Boolean(
    open &&
      item &&
      action &&
      inventorySnapshotHash &&
      !busy &&
      !inventoryBlocked &&
      !result &&
      !planMatchesOptions,
  );
  const previewRequestKey = [
    sessionKey,
    inventorySnapshotHash ?? '',
    keepData ? 'keep' : 'drop',
    conflictPolicy,
  ].join('|');
  const borrowed = Boolean(item && isPortableBorrowedRuntimeItem(item));
  const borrowedOwnerKey = item && borrowed ? portableBorrowedOwnerLabelKey(item) : null;
  const impactKey = borrowed ? borrowedImpactKey(action, item?.kind) : null;
  const storeHintKey = storeActionHintKey(action, item?.kind);
  const loadedVia =
    item?.store?.loadedViaTarget && isHubTarget(item.store.loadedViaTarget)
      ? t(`agentHub:targets.${item.store.loadedViaTarget}`)
      : item?.store?.loadedViaOtherPath
        ? t('agentHub:portable.inventory.borrowedFrom.portableStore')
        : null;

  useEffect(() => {
    if (!open) {
      previewedKeyRef.current = null;
      return;
    }
    if (!canAutoPreview || !item || !action || !inventorySnapshotHash) return;
    if (previewedKeyRef.current === previewRequestKey) return;
    previewedKeyRef.current = previewRequestKey;
    onPreview({
      inventorySnapshotHash,
      inventoryItemIds: [item.inventoryItemId],
      action,
      keepData,
      conflictPolicy,
      expectedCanonicalRevisionId: item.canonicalRevisionId,
    });
  }, [
    open,
    canAutoPreview,
    item,
    action,
    inventorySnapshotHash,
    keepData,
    conflictPolicy,
    previewRequestKey,
    onPreview,
  ]);

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

        {borrowed && item && impactKey ? (
          <StatusMessage
            tone={
              impactKey === 'borrowedImpactEnablePlugin' || impactKey === 'borrowedImpactDisablePlugin'
                ? 'info'
                : 'warn'
            }
            live="off"
            data-testid="portable-action-borrowed-impact"
          >
            {t(`agentHub:portable.actionDialog.${impactKey}`, {
              owner: borrowedOwnerKey
                ? isHubTarget(borrowedOwnerKey)
                  ? t(`agentHub:targets.${borrowedOwnerKey}`)
                  : t(`agentHub:portable.inventory.borrowedFrom.${borrowedOwnerKey}`)
                : t('agentHub:portable.inventory.borrowedFrom.unknown'),
              current: t(`agentHub:targets.${item.target}`),
            })}
            {item.ownedBy === 'sharedAgents'
              ? ` ${t('agentHub:portable.actionDialog.borrowedImpactSharedAgents')}`
              : ''}
          </StatusMessage>
        ) : null}

        {storeHintKey ? (
          <StatusMessage
            tone={action === 'destroyStore' ? 'warn' : 'info'}
            live="off"
            data-testid="portable-action-store-hint"
          >
            {t(`agentHub:portable.actionDialog.${storeHintKey}`)}
          </StatusMessage>
        ) : null}

        {item?.store?.loadedViaOtherPath && loadedVia && action === 'detach' ? (
          <StatusMessage
            tone="info"
            live="off"
            data-testid="portable-action-store-still-loaded"
          >
            {t('agentHub:portable.actionDialog.storeStillLoadedVia', {
              current: t(`agentHub:targets.${item.target}`),
              via: loadedVia,
            })}
          </StatusMessage>
        ) : null}

        {inventoryBlocked ? (
          <StatusMessage tone="warn" data-testid="portable-action-stale-banner">
            {t('agentHub:portable.actionDialog.mutationBlocked')}
          </StatusMessage>
        ) : null}

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
              <span className={styles.metaLabel}>{t(`agentHub:kinds.${item.kind}` as 'agentHub:kinds.skill')}</span>
              <span>{t(`agentHub:targets.${item.target}`)}</span>
            </div>
          </div>
        ) : null}

        {action === 'uninstall' ? (
          <label className={styles.hint} data-testid="portable-action-keep-data-label">
            <input
              type="checkbox"
              checked={keepData}
              disabled={busy}
              onChange={(event) =>
                setOptions((current) => ({
                  ...current,
                  sessionKey,
                  keepData: event.target.checked,
                }))
              }
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
              disabled={busy}
              onChange={(event) =>
                setOptions((current) => ({
                  ...current,
                  sessionKey,
                  conflictPolicy: event.target.value as 'skipExisting' | 'replaceAfterPreview',
                }))
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
        </div>
      </div>
    </Dialog>
  );
}

PortableAssetActionDialog.displayName = 'PortableAssetActionDialog';
