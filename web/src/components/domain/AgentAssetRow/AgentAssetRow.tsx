/**
 * AgentAssetRow — Agent Hub 资产列表行。
 *
 * Business Logic（为什么需要这个组件）:
 *   Hub 列表需要统一展示 instruction 资产名、策略、冲突与 Claude/Codex/OpenCode 投影状态。
 *
 * Code Logic（这个组件做什么）:
 *   组合 Card/Pill/Button/StatusMessage；交互经 props 回调委托 controller。
 */

import { useTranslation } from 'react-i18next';
import { Button, Card, Pill, StatusMessage } from '@/components/primitives';
import type {
  AgentHubAssetSummary,
  AgentHubTargetCell,
  AgentTarget,
  MaterializationStatus,
} from '@/lib/types/agentHub';
import styles from './AgentAssetRow.module.css';

const TARGET_ORDER: AgentTarget[] = ['claude', 'codex', 'opencode'];

export interface AgentAssetRowProps {
  asset: AgentHubAssetSummary;
  selected?: boolean;
  busy?: boolean;
  writeBlocked?: boolean;
  onSelect?: (asset: AgentHubAssetSummary) => void;
  onOpenBlocks?: (asset: AgentHubAssetSummary) => void;
  onOpenConflicts?: (asset: AgentHubAssetSummary) => void;
  onToggleTarget?: (
    asset: AgentHubAssetSummary,
    target: AgentTarget,
    nextEnabled: boolean,
  ) => void;
}

/**
 * Business Logic: materialization 状态映射 Pill tone。
 * Code Logic: switch 返回 success/warn/danger/neutral。
 */
function materializationTone(
  status: MaterializationStatus | null | undefined,
): 'success' | 'warn' | 'danger' | 'neutral' {
  if (!status) return 'neutral';
  switch (status) {
    case 'synced':
      return 'success';
    case 'pending':
    case 'writing':
    case 'activationRequired':
      return 'warn';
    case 'blocked':
    case 'failed':
    case 'conflict':
    case 'unsupported':
    case 'externalCollision':
      return 'danger';
    case 'drifted':
    case 'drift':
    case 'detached':
      return 'warn';
    default:
      return 'neutral';
  }
}

/**
 * Business Logic: 按固定顺序取 target cell，缺失显示 absent。
 * Code Logic: 在 asset.targets 中查找。
 */
function cellForTarget(
  asset: AgentHubAssetSummary,
  target: AgentTarget,
): AgentHubTargetCell | null {
  return asset.targets.find((cell) => cell.target === target) ?? null;
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
  onOpenConflicts,
  onToggleTarget,
}: AgentAssetRowProps) {
  const { t } = useTranslation(['agentHub', 'common']);
  const hasConflict = Boolean(asset.hasConflict);

  return (
    <Card
      variant={selected ? 'elevated' : 'outlined'}
      padding="md"
      className={styles.row}
      data-testid={`agent-asset-row-${asset.assetId}`}
      data-selected={selected || undefined}
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
            <Pill tone="neutral">{t(`agentHub:kinds.${asset.kind}`, { defaultValue: asset.kind })}</Pill>
            <Pill tone="neutral">{t(`agentHub:policy.${asset.policy}`, { defaultValue: asset.policy })}</Pill>
            {hasConflict ? (
              <Pill tone="danger" dot>
                {t('agentHub:conflict.badge')}
              </Pill>
            ) : null}
          </div>
          <div className={styles.subMeta}>
            <span>{asset.logicalKey}</span>
            <span>{asset.scopeId}</span>
            <span>{asset.originNamespace}</span>
          </div>
        </div>
        <div className={styles.actions}>
          <Button
            variant="secondary"
            size="sm"
            disabled={busy}
            onClick={() => onOpenBlocks?.(asset)}
            data-testid={`agent-asset-blocks-${asset.assetId}`}
          >
            {t('agentHub:actions.openBlocks')}
          </Button>
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
        </div>
      </div>

      <div className={styles.targets} data-testid={`agent-asset-targets-${asset.assetId}`}>
        {TARGET_ORDER.map((target) => {
          const cell = cellForTarget(asset, target);
          const enabled = cell?.desiredEnabled ?? false;
          const presence = cell?.desiredPresence ?? 'absent';
          const mat = cell?.materializationStatus ?? null;
          return (
            <div key={target} className={styles.targetCell} data-testid={`agent-target-${target}`}>
              <div className={styles.targetLabel}>
                {t(`agentHub:targets.${target}`)}
              </div>
              <Pill tone={presence === 'present' ? 'accent' : 'neutral'}>
                {t(`agentHub:presence.${presence}`)}
              </Pill>
              <Pill tone={materializationTone(mat)}>
                {mat
                  ? t(`agentHub:materialization.${mat}`, { defaultValue: String(mat) })
                  : t('agentHub:materialization.none')}
              </Pill>
              {cell?.lastError ? (
                <StatusMessage tone="danger" live="off" className={styles.cellError}>
                  {cell.lastError}
                </StatusMessage>
              ) : null}
              <Button
                variant="ghost"
                size="sm"
                disabled={busy || writeBlocked || !cell}
                loading={busy}
                onClick={() => onToggleTarget?.(asset, target, !enabled)}
                data-testid={`agent-target-toggle-${asset.assetId}-${target}`}
              >
                {enabled ? t('agentHub:actions.disableTarget') : t('agentHub:actions.enableTarget')}
              </Button>
            </div>
          );
        })}
      </div>
    </Card>
  );
}
