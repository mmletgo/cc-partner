/**
 * TargetStatusCell — 单 target 矩阵单元格 pure 视图。
 *
 * Business Logic（为什么需要这个组件）:
 *   三列矩阵需按 Gate B 状态展示 presence/enabled/mat 与显式动作。
 *
 * Code Logic（这个组件做什么）:
 *   组合 Pill/Button/StatusMessage；动作经 props 回调，禁止 import @/api/*。
 */

import { useTranslation } from 'react-i18next';
import { Button, Pill, StatusMessage } from '@/components/primitives';
import type {
  AgentHubAssetSummary,
  AgentHubTargetCell,
  AgentTarget,
} from '@/lib/types/agentHub';
import {
  blockedReason,
  canToggleEnabled,
  hasExternalCollision,
  isDetachedCell,
  isSourceTarget,
  materializationTone,
  needsActivation,
  resolveInvocationLabel,
} from './targetMatrix';
import styles from './AgentHub.module.css';

export interface TargetStatusCellProps {
  asset: AgentHubAssetSummary;
  target: AgentTarget;
  cell: AgentHubTargetCell | null;
  busy?: boolean;
  writeBlocked?: boolean;
  onToggleEnabled?: (target: AgentTarget, nextEnabled: boolean) => void;
  onRemoveTarget?: (target: AgentTarget) => void;
  onRestoreTarget?: (target: AgentTarget) => void;
  onOpenCollision?: (target: AgentTarget) => void;
}

/**
 * Business Logic: 渲染一个 CLI 目标单元格。
 * Code Logic: hooks 在 early return 前。
 */
export function TargetStatusCell({
  asset,
  target,
  cell,
  busy = false,
  writeBlocked = false,
  onToggleEnabled,
  onRemoveTarget,
  onRestoreTarget,
  onOpenCollision,
}: TargetStatusCellProps) {
  const { t } = useTranslation(['agentHub', 'common']);

  const enabled = cell?.desiredEnabled ?? false;
  const presence = cell?.desiredPresence ?? 'absent';
  const mat = cell?.materializationStatus ?? null;
  // 动作/提示按 cell mat 本地判定，禁止 row aggregate 波及无关 target
  const detached = isDetachedCell(asset, cell);
  const activation = needsActivation(asset, cell);
  const collision = hasExternalCollision(asset, cell);
  const blocked = blockedReason(asset, cell);
  const sourceOnly = Boolean(cell?.sourceOnly) || asset.aggregateStatus === 'sourceOnly';
  const showInstall = !sourceOnly || isSourceTarget(asset, target);
  const toggleOk = canToggleEnabled(asset, target, cell) && showInstall && !writeBlocked;
  const invocation = resolveInvocationLabel(asset, target);

  return (
    <div
      className={styles.targetCell}
      data-testid={`agent-target-${target}`}
      data-aggregate={asset.aggregateStatus}
    >
      <div className={styles.targetLabel}>{t(`agentHub:targets.${target}`)}</div>
      <Pill tone={presence === 'present' ? 'accent' : 'neutral'}>
        {t(`agentHub:presence.${presence}`)}
      </Pill>
      <Pill tone={materializationTone(mat)}>
        {mat
          ? t(`agentHub:materialization.${mat}`, { defaultValue: String(mat) })
          : t('agentHub:materialization.none')}
      </Pill>
      {cell?.verified ? (
        <Pill tone="success" data-testid={`agent-target-verified-${target}`}>
          {t('agentHub:matrix.verified')}
        </Pill>
      ) : null}
      {cell?.sourceOnly ? (
        <Pill tone="warn" data-testid={`agent-target-source-only-${target}`}>
          {t('agentHub:matrix.sourceOnly')}
        </Pill>
      ) : null}
      {!cell?.supported ? (
        <Pill tone="danger" data-testid={`agent-target-unsupported-${target}`}>
          {t('agentHub:matrix.unsupported')}
        </Pill>
      ) : null}
      <div className={styles.invocationMeta} data-testid={`agent-target-invocation-${target}`}>
        <span className={styles.metaLabel}>{t('agentHub:matrix.canonical')}</span>
        <span>{asset.displayName}</span>
        <span className={styles.metaLabel}>{t('agentHub:matrix.invocation')}</span>
        <span className={styles.mono}>{invocation}</span>
      </div>
      {activation ? (
        <StatusMessage tone="warn" live="off" data-testid={`agent-target-activation-${target}`}>
          {t('agentHub:matrix.activationHint')}
        </StatusMessage>
      ) : null}
      {blocked ? (
        <StatusMessage tone="danger" live="off" data-testid={`agent-target-blocked-${target}`}>
          {t('agentHub:matrix.blockedReason', { reason: blocked })}
        </StatusMessage>
      ) : null}
      {cell?.lastError && cell.lastError !== blocked ? (
        <StatusMessage tone="danger" live="off" className={styles.cellError}>
          {cell.lastError}
        </StatusMessage>
      ) : null}

      <div className={styles.targetActions}>
        {toggleOk ? (
          <Button
            variant="ghost"
            size="sm"
            disabled={busy || writeBlocked}
            loading={busy}
            onClick={() => onToggleEnabled?.(target, !enabled)}
            data-testid={`agent-target-toggle-${asset.assetId}-${target}`}
          >
            {enabled ? t('agentHub:actions.disableTarget') : t('agentHub:actions.enableTarget')}
          </Button>
        ) : null}
        {sourceOnly && !isSourceTarget(asset, target) ? (
          <span
            className={styles.hint}
            data-testid={`agent-target-no-install-${asset.assetId}-${target}`}
          >
            {t('agentHub:matrix.noInstallElsewhere')}
          </span>
        ) : null}
        {detached ? (
          <>
            <Button
              variant="secondary"
              size="sm"
              disabled={busy || writeBlocked}
              onClick={() => onRestoreTarget?.(target)}
              data-testid={`agent-target-restore-${asset.assetId}-${target}`}
            >
              {t('agentHub:actions.restoreTarget')}
            </Button>
            <Button
              variant="danger"
              size="sm"
              disabled={busy || writeBlocked}
              onClick={() => onRemoveTarget?.(target)}
              data-testid={`agent-target-remove-${asset.assetId}-${target}`}
            >
              {t('agentHub:actions.removeTarget')}
            </Button>
          </>
        ) : null}
        {collision ? (
          <Button
            variant="secondary"
            size="sm"
            disabled={busy}
            onClick={() => onOpenCollision?.(target)}
            data-testid={`agent-target-collision-${asset.assetId}-${target}`}
          >
            {t('agentHub:actions.openCollision')}
          </Button>
        ) : null}
      </div>
    </div>
  );
}
