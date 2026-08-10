/**
 * Agent Hub LAN Push Dialog — 源侧显式 peer + selection push。
 *
 * Business Logic（为什么需要这个组件）:
 *   Gate C 仅允许源设备 push；用户必须显式选择 peers 与 full/user/project/assets，
 *   且每 peer 独立报告。禁止目标 pull UI。凭据只显示 boolean 披露。
 *
 * Code Logic（这个组件做什么）:
 *   pure props 视图；hooks 仅 useTranslation/useMemo；无 @/api/*。
 */

import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Dialog, Input, Pill, StatusMessage } from '@/components/primitives';
import type {
  AgentHubLanPushPreview,
  AgentHubMultiTargetPushReport,
  AgentHubPushSelectionMode,
} from '@/lib/types/agentHub';
import styles from './AgentHub.module.css';

/**
 * 可选 peer 摘要（来自 devices 列表）。
 */
export interface LanPushPeerOption {
  deviceId: string;
  name: string;
}

/**
 * pure 视图 props。
 */
export interface LanPushDialogProps {
  open: boolean;
  busy: boolean;
  error: string | null;
  peers: LanPushPeerOption[];
  selectedPeerIds: string[];
  onTogglePeer: (deviceId: string) => void;
  mode: AgentHubPushSelectionMode;
  onModeChange: (mode: AgentHubPushSelectionMode) => void;
  assetIdsText: string;
  onAssetIdsTextChange: (value: string) => void;
  hubProjectIdsText: string;
  onHubProjectIdsTextChange: (value: string) => void;
  preview: AgentHubLanPushPreview | null;
  report: AgentHubMultiTargetPushReport | null;
  onPreview: () => void;
  onStart: () => void;
  onClose: () => void;
}

/**
 * Business Logic: pure LAN push 对话框。
 * Code Logic: Dialog + selection + per-peer report；hooks 在 early return 前。
 */
export function LanPushDialog(props: LanPushDialogProps) {
  const { t } = useTranslation(['agentHub', 'common'] as const);
  const {
    open,
    busy,
    error,
    peers,
    selectedPeerIds,
    onTogglePeer,
    mode,
    onModeChange,
    assetIdsText,
    onAssetIdsTextChange,
    hubProjectIdsText,
    onHubProjectIdsTextChange,
    preview,
    report,
    onPreview,
    onStart,
    onClose,
  } = props;

  const peerList = peers ?? [];
  const selectedPeers = useMemo(() => selectedPeerIds ?? [], [selectedPeerIds]);
  const selectedSet = useMemo(() => new Set(selectedPeers), [selectedPeers]);
  const canStart = selectedPeers.length > 0 && !busy;

  // hooks 全部在 early return 前（项目规则）
  if (!open) return null;

  return (
    <Dialog
      open={open}
      titleId="agent-hub-lan-push-title"
      onClose={busy ? () => undefined : onClose}
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
      className={styles.dialogSurface}
    >
      <div className={styles.replicationBody} data-testid="lan-push-dialog">
        <h2 id="agent-hub-lan-push-title" className={styles.drawerTitle}>
          {t('agentHub:lanPush.title')}
        </h2>
        <p className={styles.replicationHint}>{t('agentHub:lanPush.hint')}</p>
        <p className={styles.replicationDisclosure} data-testid="lan-push-plaintext-disclosure">
          {preview?.plaintextBackupDisclosure ?? t('agentHub:replication.plaintextDisclosure')}
        </p>

        <section aria-label={t('agentHub:lanPush.peersAria')}>
          <h3 className={styles.replicationSectionTitle}>{t('agentHub:lanPush.peers')}</h3>
          {peerList.length === 0 ? (
            <StatusMessage tone="warn">{t('agentHub:lanPush.noPeers')}</StatusMessage>
          ) : (
            <ul className={styles.replicationList}>
              {peerList.map((peer) => (
                <li key={peer.deviceId}>
                  <label className={styles.replicationCheckRow}>
                    <input
                      type="checkbox"
                      checked={selectedSet.has(peer.deviceId)}
                      disabled={busy}
                      onChange={() => onTogglePeer(peer.deviceId)}
                      data-testid={`lan-push-peer-${peer.deviceId}`}
                    />
                    <span>
                      {peer.name} <code>{peer.deviceId}</code>
                    </span>
                  </label>
                </li>
              ))}
            </ul>
          )}
        </section>

        <section aria-label={t('agentHub:lanPush.modeAria')}>
          <h3 className={styles.replicationSectionTitle}>{t('agentHub:lanPush.mode')}</h3>
          <div className={styles.replicationModeRow} role="radiogroup">
            {(
              [
                ['fullHub', 'agentHub:lanPush.modeFull'],
                ['userScope', 'agentHub:lanPush.modeUser'],
                ['project', 'agentHub:lanPush.modeProject'],
                ['explicitAssets', 'agentHub:lanPush.modeAssets'],
              ] as const
            ).map(([value, labelKey]) => (
              <label key={value} className={styles.replicationCheckRow}>
                <input
                  type="radio"
                  name="lan-push-mode"
                  value={value}
                  checked={mode === value}
                  disabled={busy}
                  onChange={() => onModeChange(value)}
                  data-testid={`lan-push-mode-${value}`}
                />
                <span>{t(labelKey)}</span>
              </label>
            ))}
          </div>
          {mode === 'explicitAssets' ? (
            <Input
              value={assetIdsText}
              onChange={(e) => onAssetIdsTextChange(e.target.value)}
              placeholder={t('agentHub:lanPush.assetIdsPlaceholder')}
              aria-label={t('agentHub:lanPush.assetIdsAria')}
              disabled={busy}
              data-testid="lan-push-asset-ids"
            />
          ) : null}
          {mode === 'project' ? (
            <Input
              value={hubProjectIdsText}
              onChange={(e) => onHubProjectIdsTextChange(e.target.value)}
              placeholder={t('agentHub:lanPush.projectIdsPlaceholder')}
              aria-label={t('agentHub:lanPush.projectIdsAria')}
              disabled={busy}
              data-testid="lan-push-project-ids"
            />
          ) : null}
        </section>

        {preview ? (
          <section data-testid="lan-push-preview">
            <h3 className={styles.replicationSectionTitle}>{t('agentHub:lanPush.preview')}</h3>
            <p>
              {t('agentHub:lanPush.previewSummary', {
                assets: preview.assetCount,
                revisions: preview.revisionCount,
                hash: preview.snapshotHash.slice(0, 12),
              })}
            </p>
            {preview.hasCredentialBearingAssets ? (
              <Pill tone="warn">{t('agentHub:replication.hasCredentials')}</Pill>
            ) : null}
          </section>
        ) : null}

        {report ? (
          <section data-testid="lan-push-report" aria-label={t('agentHub:lanPush.reportAria')}>
            <h3 className={styles.replicationSectionTitle}>{t('agentHub:lanPush.report')}</h3>
            <ul className={styles.replicationList}>
              {report.targets.map((target) => (
                <li key={target.peerDeviceId} data-testid={`lan-push-target-${target.peerDeviceId}`}>
                  <strong>{target.peerLabel}</strong> — {target.status}
                  {target.errorCode ? ` (${target.errorCode})` : ''}
                </li>
              ))}
            </ul>
          </section>
        ) : null}

        {error ? <StatusMessage tone="danger">{error}</StatusMessage> : null}

        <div className={styles.replicationActions}>
          <Button
            type="button"
            variant="secondary"
            onClick={onPreview}
            disabled={busy || selectedPeers.length === 0}
            data-testid="lan-push-preview-btn"
          >
            {t('agentHub:lanPush.previewAction')}
          </Button>
          <Button
            type="button"
            onClick={onStart}
            disabled={!canStart}
            loading={busy}
            data-testid="lan-push-start-btn"
          >
            {t('agentHub:lanPush.startAction')}
          </Button>
          <Button type="button" variant="ghost" onClick={onClose} disabled={busy}>
            {t('common:action.cancel')}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
