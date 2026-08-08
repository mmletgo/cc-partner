/**
 * Portable inventory 列表行（page-local，不修改 canonical AgentAssetRow）。
 *
 * Business Logic（为什么需要这个组件）:
 *   列表展示 observed 事实：名称、target、scope、实际状态、管理态与单一主动作。
 *
 * Code Logic（这个组件做什么）:
 *   pure props 视图；无 @/api；action 文案由父层经 labels 注入。
 */

import type { JSX } from 'react';
import { Button, Pill } from '@/components/primitives';
import type {
  PortableAssetActionKind,
  PortableInventoryItemDto,
} from '@/lib/types/portableInventory';
import {
  classifyPortableActualState,
  portableInventoryProblemWarnings,
  type PortableActualStateClass,
} from './portableInventoryPresentation';
import styles from './PortableInventoryRow.module.css';

export interface PortableInventoryRowLabels {
  targets: Record<'claude' | 'codex' | 'opencode', string>;
  kinds: Record<'skill' | 'command' | 'plugin' | 'mcp', string>;
  actual: Record<PortableActualStateClass, string>;
  management: Record<PortableInventoryItemDto['managementState'], string>;
  scope: Record<'user' | 'project' | 'directory', string>;
  actions: Record<PortableAssetActionKind, string>;
  sourceOrigin: Record<PortableInventoryItemDto['sourceOrigin'], string>;
}

export interface PortableInventoryRowProps {
  item: PortableInventoryItemDto;
  selected?: boolean;
  busy?: boolean;
  primaryAction: PortableAssetActionKind | null;
  labels: PortableInventoryRowLabels;
  onSelect?: (item: PortableInventoryItemDto) => void;
  onPrimaryAction?: (item: PortableInventoryItemDto, action: PortableAssetActionKind) => void;
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

/**
 * 渲染单条 portable inventory 行。
 */
export function PortableInventoryRow(props: PortableInventoryRowProps): JSX.Element {
  const {
    item,
    selected = false,
    busy = false,
    primaryAction,
    labels,
    onSelect,
    onPrimaryAction,
  } = props;
  const actual = classifyPortableActualState(item);
  const problemWarnings = portableInventoryProblemWarnings(item);
  const disabledVisual = actual === 'disabled';

  return (
    <article
      className={styles.row}
      data-testid={`portable-inventory-row-${item.inventoryItemId}`}
      data-selected={selected || undefined}
      data-disabled={disabledVisual || undefined}
      data-kind={item.kind}
      data-target={item.target}
      onClick={() => onSelect?.(item)}
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
          </div>
          {primaryAction ? (
            <div className={styles.actions}>
              <Button
                variant={primaryAction === 'uninstall' ? 'danger' : 'secondary'}
                size="sm"
                loading={busy}
                onClick={(event) => {
                  event.stopPropagation();
                  onPrimaryAction?.(item, primaryAction);
                }}
              >
                {labels.actions[primaryAction]}
              </Button>
            </div>
          ) : null}
        </div>
        {item.description ? <p className={styles.description}>{item.description}</p> : null}
        <div className={styles.metaLine}>
          <span>{labels.scope[item.scopeKind]}</span>
          <span>{labels.sourceOrigin[item.sourceOrigin]}</span>
          {item.version ? <span>{item.version}</span> : null}
        </div>
        {item.sourcePath ? <div className={styles.path}>{item.sourcePath}</div> : null}
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
    </article>
  );
}
