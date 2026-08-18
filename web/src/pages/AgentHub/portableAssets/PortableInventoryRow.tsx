/**
 * Portable inventory 列表行（page-local，不修改 canonical AgentAssetRow）。
 *
 * Business Logic（为什么需要这个组件）:
 *   列表展示 observed 事实：名称、target、scope、实际状态、管理态与行内动作集合。
 *   行内直接暴露启用/禁用/卸载等多个动作；没有详情侧栏。
 *
 * Code Logic（这个组件做什么）:
 *   pure props 视图；无 @/api；action 文案由父层经 labels 注入。
 *   名称区只读；actions 优先于 primaryAction；二者均缺时行内不渲染动作区。
 */

import type { JSX } from 'react';
import { Button, Pill } from '@/components/primitives';
import type { AgentTarget } from '@/lib/types/agentHub';
import type {
  PortableAssetActionKind,
  PortableInventoryItemDto,
} from '@/lib/types/portableInventory';
import {
  classifyPortableActualState,
  isPortableBorrowedRuntimeItem,
  needsPortableEnsureManagedRefresh,
  portableBorrowedOwnerLabelKey,
  portableInventoryProblemWarnings,
  type PortableActualStateClass,
  type PortableBorrowedOwnerLabelKey,
} from './portableInventoryPresentation';
import styles from './PortableInventoryRow.module.css';

export interface PortableInventoryRowLabels {
  targets: Partial<Record<AgentTarget, string>>;
  kinds: Record<'skill' | 'command' | 'plugin' | 'mcp', string>;
  actual: Record<PortableActualStateClass, string>;
  management: Record<PortableInventoryItemDto['managementState'], string>;
  scope: Record<'user' | 'project' | 'directory', string>;
  actions: Record<PortableAssetActionKind, string>;
  sourceOrigin: Record<PortableInventoryItemDto['sourceOrigin'], string>;
  /** 历史 unmanaged：引导刷新纳入（无 Adopt 主按钮）。 */
  unmanagedRefreshHint?: string;
  borrowedFrom: Record<PortableBorrowedOwnerLabelKey, string>;
  openInOwnerAgent: string;
  /** 便携仓库徽章；缺省时不渲染。 */
  storeBadge?: string;
}

export interface PortableInventoryRowProps {
  item: PortableInventoryItemDto;
  busy?: boolean;
  /** 行内多动作（启用/禁用/卸载等）；提供时优先于 primaryAction。 */
  actions?: PortableAssetActionKind[];
  /**
   * 行内动作点击回调（与 onPrimaryAction 并存，优先使用）。
   * 缺省时回退到 onPrimaryAction，以兼容仅传 primaryAction 的旧调用方。
   */
  onAction?: (item: PortableInventoryItemDto, action: PortableAssetActionKind) => void;
  /** 旧的单主动作（向后兼容；actions 优先）。 */
  primaryAction?: PortableAssetActionKind | null;
  labels: PortableInventoryRowLabels;
  onPrimaryAction?: (item: PortableInventoryItemDto, action: PortableAssetActionKind) => void;
  /** 借用行：切到所有者 Agent；不是 PortableAssetActionKind。 */
  onOpenOwner?: (item: PortableInventoryItemDto) => void;
}

function actualTone(
  actual: PortableActualStateClass,
): 'success' | 'neutral' | 'warn' | 'danger' {
  if (actual === 'enabled') return 'success';
  if (actual === 'problem') return 'danger';
  if (actual === 'disabled') return 'neutral';
  return 'warn';
}

function managementTone(
  state: PortableInventoryItemDto['managementState'],
): 'success' | 'neutral' | 'warn' | 'danger' {
  if (state === 'hubManaged') return 'success';
  if (state === 'drifted' || state === 'externalCollision') return 'danger';
  if (state === 'unsupported') return 'warn';
  return 'neutral';
}

/** 渲染单条 portable inventory 行。 */
export function PortableInventoryRow(props: PortableInventoryRowProps): JSX.Element {
  const {
    item,
    busy = false,
    actions,
    onAction,
    primaryAction,
    labels,
    onPrimaryAction,
    onOpenOwner,
  } = props;
  const actual = classifyPortableActualState(item);
  const problemWarnings = portableInventoryProblemWarnings(item);
  const disabledVisual = actual === 'disabled';
  const borrowed = isPortableBorrowedRuntimeItem(item);
  const borrowedOwnerKey = borrowed ? portableBorrowedOwnerLabelKey(item) : null;
  const showRefreshHint =
    !borrowed &&
    needsPortableEnsureManagedRefresh(item) &&
    Boolean(labels.unmanagedRefreshHint);
  // 借用行保留所有者徽章与「在所有者中打开」，同时按 capability 渲染启停/卸载。
  const rowActions = actions ?? (primaryAction ? [primaryAction] : []);
  const handleAction = onAction ?? onPrimaryAction;
  const showOwnerJump = borrowed && Boolean(onOpenOwner);
  const showMutations = rowActions.length > 0 && Boolean(handleAction);

  return (
    <article
      className={styles.row}
      data-testid={`portable-inventory-row-${item.inventoryItemId}`}
      data-disabled={disabledVisual || undefined}
      data-kind={item.kind}
      data-target={item.target}
      data-management={item.managementState}
    >
      <div
        className={styles.mainWrap}
        data-testid={`portable-inventory-select-${item.inventoryItemId}`}
      >
        <div className={styles.main}>
          <div className={styles.titleLine}>
            <div className={styles.titleMeta}>
              <span className={styles.name}>{item.displayName}</span>
              <Pill tone="neutral">{labels.kinds[item.kind]}</Pill>
              <Pill tone="neutral">{labels.targets[item.target]}</Pill>
              <Pill tone={actualTone(actual)} dot>
                {labels.actual[actual]}
              </Pill>
              <Pill tone={managementTone(item.managementState)}>
                {labels.management[item.managementState]}
              </Pill>
              {borrowed && borrowedOwnerKey && borrowedOwnerKey !== 'portableStore' ? (
                <Pill tone="accent" data-testid="portable-row-borrowed-badge">
                  {labels.borrowedFrom[borrowedOwnerKey]}
                </Pill>
              ) : null}
              {item.store?.storeId && labels.storeBadge ? (
                <Pill
                  tone={item.store.storeAttached ? 'success' : 'neutral'}
                  data-testid="portable-row-store-badge"
                >
                  {labels.storeBadge}
                </Pill>
              ) : null}
            </div>
          </div>
          {item.description ? <p className={styles.description}>{item.description}</p> : null}
          <div className={styles.metaLine}>
            <span>{labels.scope[item.scopeKind]}</span>
            <span>{labels.sourceOrigin[item.sourceOrigin]}</span>
            {item.version ? <span>{item.version}</span> : null}
          </div>
          {item.sourcePath ? <div className={styles.path}>{item.sourcePath}</div> : null}
          {showRefreshHint ? (
            <p className={styles.refreshHint} data-testid="portable-row-unmanaged-refresh-hint">
              {labels.unmanagedRefreshHint}
            </p>
          ) : null}
          {problemWarnings.length > 0 ? (
            <div className={styles.warnings}>
              {problemWarnings.map((warning) => (
                <span key={warning} className={styles.warning}>
                  {warning}
                </span>
              ))}
            </div>
          ) : null}
        </div>
      </div>
      {showMutations || showOwnerJump ? (
        <div className={styles.rowActions}>
          {showMutations && handleAction
            ? rowActions.map((action) => (
                <Button
                  key={action}
                  variant={
                    action === 'uninstall' || action === 'destroyStore' ? 'danger' : 'secondary'
                  }
                  size="sm"
                  loading={busy}
                  disabled={busy}
                  data-testid={`portable-row-action-${action}-${item.inventoryItemId}`}
                  onClick={() => handleAction(item, action)}
                >
                  {labels.actions[action]}
                </Button>
              ))
            : null}
          {showOwnerJump && onOpenOwner ? (
            <Button
              variant="ghost"
              size="sm"
              data-testid="portable-row-open-owner"
              onClick={() => onOpenOwner(item)}
            >
              {labels.openInOwnerAgent}
            </Button>
          ) : null}
        </div>
      ) : null}
    </article>
  );
}
