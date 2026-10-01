/**
 * 用户级镜像 Pull/Push Dialog。
 *
 * Business Logic（为什么需要这个组件）:
 *   生产 Agent Hub 一次镜像全部已登记 Agent 的用户级指令与资产；
 *   不再提供 mode radio、冲突策略或 full/user/project/assets mode；
 *   预览后可勾选同步内容（默认全选，跨 Agent 联动）。
 *
 * Code Logic（这个组件做什么）:
 *   复用 Dialog 原语；Pull 单选源设备、Push 多选对端；预览按 Agent 计数；
 *   「同步内容选择」含指令总开关与 portable 资产勾选；确认勾选门闩 apply；
 *   忙时锁 Escape/遮罩。hooks 在 early return 前。
 */

import { useMemo, type JSX } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Dialog, Pill, StatusMessage } from '@/components/primitives';
import { identityByHubTarget } from '@/lib/agentCatalog';
import type {
  UserMirrorDirection,
  UserMirrorItemState,
  UserMirrorPlanDto,
  UserMirrorResultDto,
} from '@/lib/types/userMirror';
import {
  countSelectedCredentialAssets,
  summarizeAgentPlan,
  userMirrorPlanPeers,
  userMirrorResultPeers,
  userMirrorItemStateTone,
  type UserMirrorAssetOption,
} from './userMirrorPresentation';
import styles from './UserMirrorDialog.module.css';

/** 对话框内可选对端摘要。 */
export interface UserMirrorPeerOption {
  deviceId: string;
  name: string;
}

export interface UserMirrorDialogProps {
  open: boolean;
  direction: UserMirrorDirection;
  busy: boolean;
  error: string | null;
  stale: boolean;
  devices: UserMirrorPeerOption[];
  sourceDeviceId: string;
  selectedPeerIds: string[];
  plan: UserMirrorPlanDto | null;
  result: UserMirrorResultDto | null;
  confirmed: boolean;
  canApply: boolean;
  canReconcile: boolean;
  /** 可勾选的 portable 资产（跨 Agent 去重，来自预览 plan）。 */
  assetOptions: UserMirrorAssetOption[];
  /** 当前勾选的资产键（默认全选）。 */
  selectedAssetKeys: string[];
  includeInstructions: boolean;
  onToggleAsset: (key: string) => void;
  onSelectAllAssets: () => void;
  onDeselectAllAssets: () => void;
  onSetIncludeInstructions: (value: boolean) => void;
  onSelectSourceDevice: (deviceId: string) => void;
  onTogglePeer: (deviceId: string) => void;
  onConfirmChange: (confirmed: boolean) => void;
  onPreview: () => void;
  onApply: () => void;
  onReconcile: () => void;
  onClose: () => void;
}

/**
 * Business Logic: wire state 必须显示为当前界面语言，不能把 camelCase 枚举直接暴露给用户。
 * Code Logic: 穷举映射到 i18n key。
 */
function itemStateTranslationKey(
  state: UserMirrorItemState,
):
  | 'agentHub:userMirror.itemState.succeeded'
  | 'agentHub:userMirror.itemState.failed'
  | 'agentHub:userMirror.itemState.skipped'
  | 'agentHub:userMirror.itemState.outcomeUnknown' {
  switch (state) {
    case 'succeeded':
      return 'agentHub:userMirror.itemState.succeeded';
    case 'failed':
      return 'agentHub:userMirror.itemState.failed';
    case 'skipped':
      return 'agentHub:userMirror.itemState.skipped';
    case 'outcomeUnknown':
      return 'agentHub:userMirror.itemState.outcomeUnknown';
  }
}

/**
 * Business Logic: 纯镜像确认框；LAN 无鉴权句始终可见。
 * Code Logic: Dialog + 设备选择 + Agent 计数 + 确认勾选；无 @/api/*。
 */
export function UserMirrorDialog(props: UserMirrorDialogProps): JSX.Element | null {
  const { t } = useTranslation(['agentHub', 'common'] as const);
  const {
    open,
    direction,
    busy,
    error,
    stale,
    devices,
    sourceDeviceId,
    selectedPeerIds,
    plan,
    result,
    confirmed,
    canApply,
    canReconcile,
    assetOptions,
    selectedAssetKeys,
    includeInstructions,
    onToggleAsset,
    onSelectAllAssets,
    onDeselectAllAssets,
    onSetIncludeInstructions,
    onSelectSourceDevice,
    onTogglePeer,
    onConfirmChange,
    onPreview,
    onApply,
    onReconcile,
    onClose,
  } = props;

  const selectedSet = useMemo(() => new Set(selectedPeerIds), [selectedPeerIds]);
  const selectedAssetSet = useMemo(() => new Set(selectedAssetKeys), [selectedAssetKeys]);
  const planPeers = useMemo(() => userMirrorPlanPeers(plan), [plan]);
  const resultPeers = useMemo(() => userMirrorResultPeers(result), [result]);
  const peerNames = useMemo(
    () => new Map(devices.map((peer) => [peer.deviceId, peer.name])),
    [devices],
  );
  const titleKey = direction === 'pull' ? 'agentHub:userMirror.pullTitle' : 'agentHub:userMirror.pushTitle';
  const hintKey = direction === 'pull' ? 'agentHub:userMirror.pullHint' : 'agentHub:userMirror.pushHint';
  const canPreview =
    !busy && (direction === 'pull' ? Boolean(sourceDeviceId) : selectedSet.size > 0);
  const selectionLocked =
    busy || Boolean(result) || canReconcile || (Boolean(plan) && confirmed && !canApply);
  const emptySelection = Boolean(plan) && !includeInstructions && selectedAssetSet.size === 0;
  const hasIncompleteResult = Boolean(result) && canReconcile;

  if (!open) return null;

  return (
    <Dialog
      open={open}
      titleId="user-mirror-title"
      onClose={busy ? () => undefined : onClose}
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
      className={styles.dialog}
    >
      <div className={styles.body} data-testid="user-mirror-dialog">
        <div className={styles.scrollRegion} data-testid="user-mirror-scroll-region">
          <h2 id="user-mirror-title" className={styles.title}>
            {t(titleKey)}
          </h2>
          <p className={styles.hint}>{t(hintKey)}</p>
          <p className={styles.disclosure} data-testid="user-mirror-lan-risk">
            {t('agentHub:userMirror.lanNoAuthRisk')}
          </p>

          {direction === 'pull' ? (
            <section className={styles.section} aria-label={t('agentHub:userMirror.sourceDeviceAria')}>
              <h3 className={styles.sectionTitle}>{t('agentHub:userMirror.sourceDevice')}</h3>
              {devices.length === 0 ? (
                <StatusMessage tone="warn">{t('agentHub:userMirror.noPeers')}</StatusMessage>
              ) : (
                <ul className={styles.list}>
                  {devices.map((peer) => (
                    <li key={peer.deviceId}>
                      <label className={styles.checkRow}>
                        <input
                          type="radio"
                          name="user-mirror-source"
                          checked={sourceDeviceId === peer.deviceId}
                          disabled={busy}
                          onChange={() => onSelectSourceDevice(peer.deviceId)}
                          data-testid={`user-mirror-source-${peer.deviceId}`}
                        />
                        <span>
                          {peer.name} <span className={styles.peerMeta}>{peer.deviceId}</span>
                        </span>
                      </label>
                    </li>
                  ))}
                </ul>
              )}
            </section>
          ) : (
            <section className={styles.section} aria-label={t('agentHub:userMirror.peersAria')}>
              <h3 className={styles.sectionTitle}>{t('agentHub:userMirror.peers')}</h3>
              {devices.length === 0 ? (
                <StatusMessage tone="warn">{t('agentHub:userMirror.noPeers')}</StatusMessage>
              ) : (
                <ul className={styles.list}>
                  {devices.map((peer) => (
                    <li key={peer.deviceId}>
                      <label className={styles.checkRow}>
                        <input
                          type="checkbox"
                          checked={selectedSet.has(peer.deviceId)}
                          disabled={busy}
                          onChange={() => onTogglePeer(peer.deviceId)}
                          data-testid={`user-mirror-peer-${peer.deviceId}`}
                        />
                        <span>
                          {peer.name} <span className={styles.peerMeta}>{peer.deviceId}</span>
                        </span>
                      </label>
                    </li>
                  ))}
                </ul>
              )}
            </section>
          )}

          {plan ? (
            <section className={styles.section} data-testid="user-mirror-plan">
              <h3 className={styles.sectionTitle}>{t('agentHub:userMirror.planTitle')}</h3>
              <div className={styles.peerCards}>
                {planPeers.map((peerPlan) => {
                  const peerName = peerNames.get(
                    direction === 'pull' ? plan.sourceDeviceId : peerPlan.destinationDeviceId,
                  );
                  const directionLabel =
                    direction === 'pull'
                      ? t('agentHub:userMirror.directionFromPeer', {
                          peer: peerName ?? plan.sourceDeviceId,
                        })
                      : t('agentHub:userMirror.directionToPeer', {
                          peer: peerName ?? peerPlan.destinationDeviceId,
                        });
                  const agentRows = peerPlan.agents.map((agent) =>
                    summarizeAgentPlan(agent, {
                      includeInstructions,
                      selectedAssetKeys: selectedAssetSet,
                    }),
                  );
                  const credentialCount = countSelectedCredentialAssets(
                    peerPlan.agents,
                    selectedAssetSet,
                  );
                  return (
                    <article
                      key={peerPlan.destinationDeviceId}
                      className={styles.peerCard}
                      data-testid={`user-mirror-peer-plan-${peerPlan.destinationDeviceId}`}
                    >
                      <div className={styles.peerCardHeader}>
                        <strong>{directionLabel}</strong>
                        <span className={styles.peerMeta}>{peerPlan.destinationDeviceId}</span>
                      </div>
                      {credentialCount > 0 ? (
                        <Pill
                          tone="warn"
                          data-testid={
                            planPeers.length === 1
                              ? 'user-mirror-credentials'
                              : `user-mirror-credentials-${peerPlan.destinationDeviceId}`
                          }
                        >
                          {t('agentHub:userMirror.credentials', { count: credentialCount })}
                        </Pill>
                      ) : (
                        <span
                          data-testid={
                            planPeers.length === 1
                              ? 'user-mirror-credentials'
                              : `user-mirror-credentials-${peerPlan.destinationDeviceId}`
                          }
                          hidden
                        />
                      )}
                      <ul className={styles.list}>
                        {agentRows.map((row) => (
                          <li
                            key={row.target}
                            className={styles.agentRow}
                            data-testid={`user-mirror-agent-${row.target}`}
                          >
                            <span className={styles.agentName}>
                              {identityByHubTarget(row.target)?.displayName ?? row.target}
                            </span>
                            <span className={styles.agentCounts}>
                              {t('agentHub:userMirror.agentCounts', {
                                writes: row.writes,
                                upserts: row.upserts,
                                deletes: row.deletes,
                                disables: row.disables,
                              })}
                            </span>
                          </li>
                        ))}
                      </ul>
                      {peerPlan.blockingReasons.length > 0 ? (
                        <StatusMessage tone="danger">
                          {t('agentHub:userMirror.blocked', {
                            reasons: peerPlan.blockingReasons.join(' · '),
                          })}
                        </StatusMessage>
                      ) : null}
                    </article>
                  );
                })}
              </div>
            </section>
          ) : null}

          {plan ? (
            <section
              className={styles.section}
              data-testid="user-mirror-selection"
              aria-label={t('agentHub:userMirror.selectionAria')}
            >
              <h3 className={styles.sectionTitle}>{t('agentHub:userMirror.selectionTitle')}</h3>
              <label className={styles.checkRow}>
                <input
                  type="checkbox"
                  checked={includeInstructions}
                  disabled={selectionLocked}
                  onChange={(event) => onSetIncludeInstructions(event.currentTarget.checked)}
                  data-testid="user-mirror-include-instructions"
                />
                <span>{t('agentHub:userMirror.includeInstructions')}</span>
              </label>
              {assetOptions.length > 0 ? (
                <>
                  <div className={styles.assetActions}>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={onSelectAllAssets}
                      disabled={selectionLocked}
                      data-testid="user-mirror-asset-select-all"
                    >
                      {t('agentHub:userMirror.assetSelectAll')}
                    </Button>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={onDeselectAllAssets}
                      disabled={selectionLocked}
                      data-testid="user-mirror-asset-deselect-all"
                    >
                      {t('agentHub:userMirror.assetDeselectAll')}
                    </Button>
                  </div>
                  <ul className={styles.list}>
                    {assetOptions.map((option) => (
                      <li key={option.key}>
                        <label className={styles.checkRow}>
                          <input
                            type="checkbox"
                            checked={selectedAssetSet.has(option.key)}
                            disabled={selectionLocked}
                            onChange={() => onToggleAsset(option.key)}
                            data-testid={`user-mirror-asset-${option.key}`}
                          />
                          <span>
                            {option.displayName}{' '}
                            <span className={styles.peerMeta}>({option.kind})</span>
                          </span>
                        </label>
                      </li>
                    ))}
                  </ul>
                </>
              ) : null}
              {emptySelection ? (
                <StatusMessage tone="warn" data-testid="user-mirror-empty-selection">
                  {t('agentHub:userMirror.selectionRequired')}
                </StatusMessage>
              ) : null}
            </section>
          ) : null}

          <label className={styles.confirmRow}>
            <input
              type="checkbox"
              checked={confirmed}
              disabled={selectionLocked || !plan || emptySelection}
              onChange={(event) => onConfirmChange(event.currentTarget.checked)}
              data-testid="user-mirror-confirm-overwrite"
              aria-label={t('agentHub:userMirror.confirmAria')}
            />
            <span>{t('agentHub:userMirror.confirmOverwrite')}</span>
          </label>

          {stale ? (
            <StatusMessage tone="warn" data-testid="user-mirror-stale">
              {t('agentHub:userMirror.stale')}
            </StatusMessage>
          ) : null}

          {error ? (
            <StatusMessage tone="danger" data-testid="user-mirror-error">
              {t('agentHub:userMirror.errorWithDetail', { detail: error })}
            </StatusMessage>
          ) : null}

          {canReconcile && !result ? (
            <StatusMessage tone="warn" data-testid="user-mirror-outcome-unknown">
              {t('agentHub:userMirror.outcomeUnknown')}
            </StatusMessage>
          ) : null}

          {hasIncompleteResult ? (
            <StatusMessage tone="warn" data-testid="user-mirror-partial">
              {t('agentHub:userMirror.partial')}
            </StatusMessage>
          ) : result ? (
            <StatusMessage tone="success" data-testid="user-mirror-success">
              {t('agentHub:userMirror.completed')}
            </StatusMessage>
          ) : null}

          {result ? (
            <section
              className={styles.section}
              data-testid="user-mirror-report"
              aria-label={t('agentHub:userMirror.reportAria')}
            >
              <h3 className={styles.sectionTitle}>{t('agentHub:userMirror.reportTitle')}</h3>
              <ul className={styles.peerCards}>
                {resultPeers.map((peerResult) => {
                  const peerName = peerNames.get(
                    direction === 'pull' ? result.sourceDeviceId : peerResult.destinationDeviceId,
                  );
                  const directionLabel =
                    direction === 'pull'
                      ? t('agentHub:userMirror.directionFromPeer', {
                          peer: peerName ?? result.sourceDeviceId,
                        })
                      : t('agentHub:userMirror.directionToPeer', {
                          peer: peerName ?? peerResult.destinationDeviceId,
                        });
                  return (
                    <li
                      key={peerResult.destinationDeviceId}
                      className={styles.peerCard}
                      data-testid={`user-mirror-report-${peerResult.destinationDeviceId}`}
                    >
                      <div className={styles.peerCardHeader}>
                        <strong>{directionLabel}</strong>
                        <span className={styles.peerMeta}>{peerResult.destinationDeviceId}</span>
                      </div>
                      <ul className={styles.resultList}>
                        {peerResult.agents.map((agent) => (
                          <li key={`${peerResult.destinationDeviceId}-${agent.target}`}>
                            <Pill tone={userMirrorItemStateTone(agent.state)}>
                              {identityByHubTarget(agent.target)?.displayName ?? agent.target}
                              {` · ${t(itemStateTranslationKey(agent.state))}`}
                            </Pill>
                            {agent.errorCode ? (
                              <span className={styles.resultDetail}>
                                {t('agentHub:userMirror.errorCode', { code: agent.errorCode })}
                              </span>
                            ) : null}
                            {agent.message ? (
                              <span className={styles.resultDetail}>
                                {t('agentHub:userMirror.errorMessage', { message: agent.message })}
                              </span>
                            ) : null}
                          </li>
                        ))}
                      </ul>
                    </li>
                  );
                })}
              </ul>
            </section>
          ) : null}
        </div>

        <div className={styles.footer} data-testid="user-mirror-footer">
          <div className={styles.actions}>
            <Button
              type="button"
              variant="secondary"
              onClick={onPreview}
              disabled={!canPreview}
              data-testid="user-mirror-preview"
            >
              {t('agentHub:userMirror.previewAction')}
            </Button>
            <Button
              type="button"
              variant="primary"
              onClick={onApply}
              disabled={!canApply}
              loading={busy}
              data-testid="user-mirror-apply"
            >
              {t('agentHub:userMirror.applyAction')}
            </Button>
            {canReconcile ? (
              <Button
                type="button"
                variant="secondary"
                onClick={onReconcile}
                disabled={busy}
                data-testid="user-mirror-reconcile"
              >
                {t('agentHub:userMirror.reconcileAction')}
              </Button>
            ) : null}
            <Button type="button" variant="ghost" onClick={onClose} disabled={busy}>
              {t('common:action.cancel')}
            </Button>
          </div>
        </div>
      </div>
    </Dialog>
  );
}
