/**
 * 用户级镜像 Pull/Push Dialog。
 *
 * Business Logic（为什么需要这个组件）:
 *   生产 Agent Hub 一次镜像全部已登记 Agent 的用户级指令与资产；
 *   不再提供条目勾选、kind 筛选、冲突策略或 full/user/project/assets mode。
 *
 * Code Logic（这个组件做什么）:
 *   复用 Dialog 原语；Pull 单选源设备、Push 多选对端；预览按 Agent 计数；
 *   确认勾选门闩 apply；忙时锁 Escape/遮罩。hooks 在 early return 前。
 */

import { useMemo, type JSX } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Dialog, Pill, StatusMessage } from '@/components/primitives';
import { identityByHubTarget } from '@/lib/agentCatalog';
import type { UserMirrorDirection, UserMirrorPlanDto, UserMirrorResultDto } from '@/lib/types/userMirror';
import {
  summarizePlanAgents,
  userMirrorItemStateTone,
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
  onSelectSourceDevice: (deviceId: string) => void;
  onTogglePeer: (deviceId: string) => void;
  onConfirmChange: (confirmed: boolean) => void;
  onPreview: () => void;
  onApply: () => void;
  onReconcile: () => void;
  onClose: () => void;
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
    onSelectSourceDevice,
    onTogglePeer,
    onConfirmChange,
    onPreview,
    onApply,
    onReconcile,
    onClose,
  } = props;

  const peerList = devices ?? [];
  const selectedSet = useMemo(() => new Set(selectedPeerIds ?? []), [selectedPeerIds]);
  const agentRows = useMemo(() => summarizePlanAgents(plan), [plan]);
  const titleKey = direction === 'pull' ? 'agentHub:userMirror.pullTitle' : 'agentHub:userMirror.pushTitle';
  const hintKey = direction === 'pull' ? 'agentHub:userMirror.pullHint' : 'agentHub:userMirror.pushHint';
  const canPreview =
    !busy && (direction === 'pull' ? Boolean(sourceDeviceId) : selectedSet.size > 0);

  if (!open) return null;

  return (
    <Dialog
      open={open}
      titleId="user-mirror-title"
      onClose={busy ? () => undefined : onClose}
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
    >
      <div className={styles.body} data-testid="user-mirror-dialog">
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
            {peerList.length === 0 ? (
              <StatusMessage tone="warn">{t('agentHub:userMirror.noPeers')}</StatusMessage>
            ) : (
              <ul className={styles.list}>
                {peerList.map((peer) => (
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
            {peerList.length === 0 ? (
              <StatusMessage tone="warn">{t('agentHub:userMirror.noPeers')}</StatusMessage>
            ) : (
              <ul className={styles.list}>
                {peerList.map((peer) => (
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
            {plan.hasCredentialBearingAssets ? (
              <Pill tone="warn" data-testid="user-mirror-credentials">
                {t('agentHub:userMirror.credentials', { count: plan.credentialBearingCount })}
              </Pill>
            ) : (
              <span data-testid="user-mirror-credentials" hidden />
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
          </section>
        ) : null}

        <label className={styles.confirmRow}>
          <input
            type="checkbox"
            checked={confirmed}
            disabled={busy || !plan}
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
            {error}
          </StatusMessage>
        ) : null}

        {result?.partial ? (
          <StatusMessage tone="warn" data-testid="user-mirror-partial">
            {t('agentHub:userMirror.partial')}
          </StatusMessage>
        ) : null}

        {result ? (
          <section
            className={styles.section}
            data-testid="user-mirror-report"
            aria-label={t('agentHub:userMirror.reportAria')}
          >
            <h3 className={styles.sectionTitle}>{t('agentHub:userMirror.reportTitle')}</h3>
            <ul className={styles.list}>
              <li data-testid={`user-mirror-report-${result.destinationDeviceId}`}>
                <span className={styles.peerMeta}>{result.destinationDeviceId}</span>
                {result.agents.map((agent) => (
                  <Pill
                    key={`${result.destinationDeviceId}-${agent.target}`}
                    tone={userMirrorItemStateTone(agent.state)}
                  >
                    {identityByHubTarget(agent.target)?.displayName ?? agent.target}
                    {` ${agent.state}`}
                  </Pill>
                ))}
              </li>
            </ul>
          </section>
        ) : null}

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
    </Dialog>
  );
}
