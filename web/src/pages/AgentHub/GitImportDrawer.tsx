/**
 * Agent Hub Git Import Drawer — inspect / preview / confirm 分离。
 *
 * Business Logic（为什么需要这个组件）:
 *   远端 device lane 永不自动导入；用户先 inspect，再 preview，再 confirm。
 *   unmapped project 必须显式 mapping，opt-in 另走现有 project preview。
 *   凭据只显示 boolean；无 secret 正文。
 *
 * Code Logic（这个组件做什么）:
 *   pure props 视图；三阶段 UI；hooks 在 early return 前。
 */

import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Drawer, Input, Pill, StatusMessage } from '@/components/primitives';
import type {
  AgentHubConfirmGitImportOutcome,
  AgentHubGitImportPreview,
  AgentHubGitLaneInspectReport,
  AgentHubResolvedProjectMapping,
} from '@/lib/types/agentHub';
import styles from './AgentHub.module.css';

/**
 * pure 视图 props。
 */
export interface GitImportDrawerProps {
  open: boolean;
  busy: boolean;
  error: string | null;
  inspectReport: AgentHubGitLaneInspectReport | null;
  selectedLaneDeviceId: string | null;
  preview: AgentHubGitImportPreview | null;
  selectedAssetIds: string[];
  /**
   * 是否已经把隐式“全选”物化为显式集合；显式集合可以合法为空，但此时禁止确认。
   */
  hasExplicitAssetSelection?: boolean;
  mappingDrafts: Record<string, string>;
  confirmOutcome: AgentHubConfirmGitImportOutcome | null;
  lastMapping: AgentHubResolvedProjectMapping | null;
  onInspect: () => void;
  onSelectLane: (laneDeviceId: string) => void;
  onPreview: () => void;
  onToggleAsset: (assetId: string) => void;
  onMappingDraftChange: (hubProjectId: string, localProjectId: string) => void;
  onConfirmMapping: (hubProjectId: string) => void;
  onConfirmImport: () => void;
  onClose: () => void;
}

/**
 * Business Logic: pure Git import drawer。
 * Code Logic: inspect → preview → confirm 分步；hooks 在 early return 前。
 */
export function GitImportDrawer(props: GitImportDrawerProps) {
  const { t } = useTranslation(['agentHub', 'common'] as const);
  const {
    open,
    busy,
    error,
    inspectReport,
    selectedLaneDeviceId,
    preview,
    selectedAssetIds,
    hasExplicitAssetSelection = false,
    mappingDrafts,
    confirmOutcome,
    lastMapping,
    onInspect,
    onSelectLane,
    onPreview,
    onToggleAsset,
    onMappingDraftChange,
    onConfirmMapping,
    onConfirmImport,
    onClose,
  } = props;

  const selectedAssets = selectedAssetIds;
  const drafts = mappingDrafts ?? {};
  const selectedAssetSet = useMemo(() => new Set(selectedAssets), [selectedAssets]);

  // hooks 全部在 early return 前
  if (!open) return null;

  const unmapped =
    preview?.projectCandidates.filter((c) => !c.localWorkbenchProjectId) ?? [];

  return (
    <Drawer
      open={open}
      titleId="agent-hub-git-import-title"
      onClose={busy ? () => undefined : onClose}
      side="right"
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
      className={styles.drawerSurface}
    >
      <div className={styles.replicationBody} data-testid="git-import-drawer">
        <h2 id="agent-hub-git-import-title" className={styles.drawerTitle}>
          {t('agentHub:gitImport.title')}
        </h2>
        <p className={styles.replicationHint}>{t('agentHub:gitImport.hint')}</p>
        <p className={styles.replicationDisclosure} data-testid="git-import-plaintext-disclosure">
          {preview?.plaintextBackupDisclosure ?? t('agentHub:replication.plaintextDisclosure')}
        </p>

        <section data-testid="git-import-inspect-step">
          <h3 className={styles.replicationSectionTitle}>{t('agentHub:gitImport.stepInspect')}</h3>
          <Button
            type="button"
            variant="secondary"
            onClick={onInspect}
            disabled={busy}
            data-testid="git-import-inspect-btn"
          >
            {t('agentHub:gitImport.inspectAction')}
          </Button>
          {inspectReport ? (
            <ul className={styles.replicationList} data-testid="git-import-lane-list">
              {inspectReport.lanes.map((lane) => (
                <li key={lane.laneDeviceId}>
                  <button
                    type="button"
                    className={styles.replicationLaneBtn}
                    data-testid={`git-import-lane-${lane.laneDeviceId}`}
                    disabled={busy || lane.status !== 'ok'}
                    onClick={() => onSelectLane(lane.laneDeviceId)}
                    aria-pressed={selectedLaneDeviceId === lane.laneDeviceId}
                  >
                    <strong>{lane.laneDeviceId}</strong> — {lane.status}
                    {lane.snapshotHash
                      ? ` · ${lane.snapshotHash.slice(0, 12)}`
                      : lane.errorCode
                        ? ` · ${lane.errorCode}`
                        : ''}
                  </button>
                </li>
              ))}
            </ul>
          ) : null}
        </section>

        <section data-testid="git-import-preview-step">
          <h3 className={styles.replicationSectionTitle}>{t('agentHub:gitImport.stepPreview')}</h3>
          <Button
            type="button"
            variant="secondary"
            onClick={onPreview}
            disabled={busy || !selectedLaneDeviceId}
            data-testid="git-import-preview-btn"
          >
            {t('agentHub:gitImport.previewAction')}
          </Button>
          {preview ? (
            <div data-testid="git-import-preview">
              <p>
                {t('agentHub:gitImport.previewSummary', {
                  added: preview.changeCounts.added,
                  modified: preview.changeCounts.modified,
                  deleted: preview.changeCounts.deleted,
                  conflict: preview.changeCounts.conflict,
                  hash: preview.snapshotHash.slice(0, 12),
                })}
              </p>
              {preview.hasCredentialBearingAssets ? (
                <Pill tone="warn">{t('agentHub:replication.hasCredentials')}</Pill>
              ) : null}
              <ul className={styles.replicationList}>
                {preview.assets.map((asset) => (
                  <li key={asset.assetId}>
                    <label className={styles.replicationCheckRow}>
                      <input
                        type="checkbox"
                        checked={
                          !hasExplicitAssetSelection || selectedAssetSet.has(asset.assetId)
                        }
                        disabled={busy}
                        onChange={() => onToggleAsset(asset.assetId)}
                        data-testid={`git-import-asset-${asset.assetId}`}
                      />
                      <span>
                        {asset.displayName} ({asset.changeKind})
                        {asset.hasCredential ? ` · ${t('agentHub:replication.credentialLabel')}` : ''}
                      </span>
                    </label>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
        </section>

        <section data-testid="git-import-mapping-step">
          <h3 className={styles.replicationSectionTitle}>{t('agentHub:gitImport.stepMapping')}</h3>
          {unmapped.length === 0 ? (
            <p>{t('agentHub:gitImport.noUnmapped')}</p>
          ) : (
            <ul className={styles.replicationList}>
              {unmapped.map((candidate) => (
                <li key={candidate.hubProjectId}>
                  <div className={styles.replicationMapRow}>
                    <code>{candidate.hubProjectId}</code>
                    <Input
                      value={drafts[candidate.hubProjectId] ?? ''}
                      onChange={(e) =>
                        onMappingDraftChange(candidate.hubProjectId, e.target.value)
                      }
                      placeholder={t('agentHub:gitImport.localProjectPlaceholder')}
                      aria-label={t('agentHub:gitImport.localProjectAria', {
                        hub: candidate.hubProjectId,
                      })}
                      disabled={busy}
                      data-testid={`git-import-map-${candidate.hubProjectId}`}
                    />
                    <Button
                      type="button"
                      variant="secondary"
                      disabled={busy || !(drafts[candidate.hubProjectId] ?? '').trim()}
                      onClick={() => onConfirmMapping(candidate.hubProjectId)}
                      data-testid={`git-import-map-confirm-${candidate.hubProjectId}`}
                    >
                      {t('agentHub:gitImport.mapAction')}
                    </Button>
                  </div>
                  <p className={styles.replicationHint}>{t('agentHub:gitImport.mapThenOptIn')}</p>
                </li>
              ))}
            </ul>
          )}
          {lastMapping ? (
            <StatusMessage tone="success" data-testid="git-import-last-mapping">
              {t('agentHub:gitImport.mappingSaved', {
                hub: lastMapping.hubProjectId,
                optedIn: lastMapping.optedIn ? 'yes' : 'no',
              })}
            </StatusMessage>
          ) : null}
        </section>

        <section data-testid="git-import-confirm-step">
          <h3 className={styles.replicationSectionTitle}>{t('agentHub:gitImport.stepConfirm')}</h3>
          <Button
            type="button"
            onClick={onConfirmImport}
            disabled={
              busy ||
              !preview ||
              (hasExplicitAssetSelection && selectedAssets.length === 0)
            }
            loading={busy}
            data-testid="git-import-confirm-btn"
          >
            {t('agentHub:gitImport.confirmAction')}
          </Button>
          {confirmOutcome ? (
            <StatusMessage tone="success" data-testid="git-import-outcome">
              {t('agentHub:gitImport.confirmSummary', {
                assets: confirmOutcome.import.importedAssetIds.length,
                revisions: confirmOutcome.import.insertedRevisions,
                projections: confirmOutcome.import.projectionsScheduled,
              })}
            </StatusMessage>
          ) : null}
        </section>

        {error ? (
          <StatusMessage tone="danger" data-testid="git-import-error">
            {error}
          </StatusMessage>
        ) : null}

        <div className={styles.replicationActions}>
          <Button type="button" variant="ghost" onClick={onClose} disabled={busy}>
            {t('common:action.cancel')}
          </Button>
        </div>
      </div>
    </Drawer>
  );
}
