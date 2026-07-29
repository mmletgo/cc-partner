/**
 * AgentAssetRow — Agent Hub 资产列表行。
 *
 * Business Logic（为什么需要这个组件）:
 *   Hub 列表需要统一展示 instruction/portable 资产名、策略、冲突、
 *   aggregate 汇总与 Claude/Codex/OpenCode 投影状态。
 *
 * Code Logic（这个组件做什么）:
 *   组合 Card/Pill/Button + TargetStatusCell；交互经 props 回调委托 controller。
 */

import { useTranslation } from 'react-i18next';
import { Button, Card, Pill } from '@/components/primitives';
import type {
  AgentHubAssetSummary,
  AgentTarget,
} from '@/lib/types/agentHub';
import { TargetStatusCell } from '@/pages/AgentHub/TargetStatusCell';
import {
  AGENT_TARGET_ORDER,
  aggregateTone,
  cellForTarget,
  listPartialReasons,
} from '@/pages/AgentHub/targetMatrix';
import styles from './AgentAssetRow.module.css';

export interface AgentAssetRowProps {
  asset: AgentHubAssetSummary;
  selected?: boolean;
  busy?: boolean;
  writeBlocked?: boolean;
  onSelect?: (asset: AgentHubAssetSummary) => void;
  onOpenBlocks?: (asset: AgentHubAssetSummary) => void;
  onOpenPlugin?: (asset: AgentHubAssetSummary) => void;
  onOpenConflicts?: (asset: AgentHubAssetSummary) => void;
  onToggleTarget?: (
    asset: AgentHubAssetSummary,
    target: AgentTarget,
    nextEnabled: boolean,
  ) => void;
  onRemoveTarget?: (asset: AgentHubAssetSummary, target: AgentTarget) => void;
  onRestoreTarget?: (asset: AgentHubAssetSummary, target: AgentTarget) => void;
  onOpenCollision?: (asset: AgentHubAssetSummary, target: AgentTarget) => void;
  onDeleteEverywhere?: (asset: AgentHubAssetSummary) => void;
}

/**
 * 渲染单个 Agent Hub 资产行。
 */
export function AgentAssetRow({
  asset,
  selected = false,
  busy = false,
  writeBlocked = false,
  onSelect,
  onOpenBlocks,
  onOpenPlugin,
  onOpenConflicts,
  onToggleTarget,
  onRemoveTarget,
  onRestoreTarget,
  onOpenCollision,
  onDeleteEverywhere,
}: AgentAssetRowProps) {
  const { t } = useTranslation(['agentHub', 'common']);
  const hasConflict = Boolean(asset.hasConflict);
  const isPlugin = asset.kind === 'plugin';
  const partialReasons =
    asset.aggregateStatus === 'partial' ? listPartialReasons(asset) : [];

  return (
    <Card
      variant={selected ? 'elevated' : 'outlined'}
      padding="md"
      className={styles.row}
      data-testid={`agent-asset-row-${asset.assetId}`}
      data-selected={selected || undefined}
      data-aggregate={asset.aggregateStatus}
    >
      <div className={styles.header}>
        <div className={styles.titleBlock}>
          <button
            type="button"
            className={styles.titleButton}
            onClick={() => onSelect?.(asset)}
            data-testid={`agent-asset-select-${asset.assetId}`}
          >
            <span className={styles.name}>{asset.displayName}</span>
          </button>
          <div className={styles.meta}>
            <Pill tone="neutral">
              {t(`agentHub:kinds.${asset.kind}`, { defaultValue: asset.kind })}
            </Pill>
            <Pill tone="neutral">
              {t(`agentHub:policy.${asset.policy}`, { defaultValue: asset.policy })}
            </Pill>
            <Pill
              tone={aggregateTone(asset.aggregateStatus)}
              data-testid={`agent-asset-aggregate-${asset.assetId}`}
            >
              {t(`agentHub:aggregate.${asset.aggregateStatus}`)}
            </Pill>
            {hasConflict ? (
              <Pill tone="danger" dot>
                {t('agentHub:conflict.badge')}
              </Pill>
            ) : null}
          </div>
          <div className={styles.subMeta}>
            <span data-testid={`agent-asset-canonical-${asset.assetId}`}>{asset.displayName}</span>
            <span>{asset.logicalKey}</span>
            <span>{asset.scopeId}</span>
            <span>{asset.originNamespace}</span>
          </div>
          {partialReasons.length > 0 ? (
            <ul
              className={styles.partialList}
              data-testid={`agent-asset-partial-${asset.assetId}`}
            >
              {partialReasons.map((reason) => (
                <li key={reason}>{reason}</li>
              ))}
            </ul>
          ) : null}
        </div>
        <div className={styles.actions}>
          {isPlugin ? (
            <Button
              variant="secondary"
              size="sm"
              disabled={busy}
              onClick={() => onOpenPlugin?.(asset)}
              data-testid={`agent-asset-plugin-${asset.assetId}`}
            >
              {t('agentHub:actions.openPluginComponents')}
            </Button>
          ) : (
            <Button
              variant="secondary"
              size="sm"
              disabled={busy}
              onClick={() => onOpenBlocks?.(asset)}
              data-testid={`agent-asset-blocks-${asset.assetId}`}
            >
              {t('agentHub:actions.openBlocks')}
            </Button>
          )}
          {hasConflict ? (
            <Button
              variant="danger"
              size="sm"
              disabled={busy || writeBlocked}
              onClick={() => onOpenConflicts?.(asset)}
              data-testid={`agent-asset-conflicts-${asset.assetId}`}
            >
              {t('agentHub:actions.openConflicts')}
            </Button>
          ) : null}
          <Button
            variant="danger"
            size="sm"
            disabled={busy || writeBlocked}
            onClick={() => onDeleteEverywhere?.(asset)}
            data-testid={`agent-asset-delete-everywhere-${asset.assetId}`}
          >
            {t('agentHub:actions.deleteEverywhere')}
          </Button>
        </div>
      </div>

      <div className={styles.targets} data-testid={`agent-asset-targets-${asset.assetId}`}>
        {AGENT_TARGET_ORDER.map((target) => (
          <TargetStatusCell
            key={target}
            asset={asset}
            target={target}
            cell={cellForTarget(asset, target)}
            busy={busy}
            writeBlocked={writeBlocked}
            onToggleEnabled={(tgt, next) => onToggleTarget?.(asset, tgt, next)}
            onRemoveTarget={(tgt) => onRemoveTarget?.(asset, tgt)}
            onRestoreTarget={(tgt) => onRestoreTarget?.(asset, tgt)}
            onOpenCollision={(tgt) => onOpenCollision?.(asset, tgt)}
          />
        ))}
      </div>
    </Card>
  );
}
