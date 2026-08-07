/**
 * SkillDetails — Skill 资产专属详情。
 *
 * Business Logic（为什么需要这个组件）:
 *   Skill 需展示目录树 hash、standalone/plugin 来源与 invocation，不能复用 Instruction Blocks。
 *
 * Code Logic（这个组件做什么）:
 *   pure props 渲染；不 import @/api/*。
 */

import type { PortableInventoryItemDto } from '@/lib/types/portableInventory';
import styles from '../AgentHub.module.css';

export interface SkillDetailsProps {
  item: PortableInventoryItemDto;
  labels: {
    treeHash: string;
    origin: string;
    invocation: string;
    sourcePath: string;
    description: string;
    missing: string;
    parentPlugin: string;
  };
}

/**
 * Business Logic: Skill 详情事实面。
 * Code Logic: data-testid 供合同测试锁定。
 */
export function SkillDetails({ item, labels }: SkillDetailsProps) {
  return (
    <section className={styles.drawerSection} data-testid="portable-skill-details">
      <div className={styles.metaBlock}>
        <div>
          <span className={styles.metaLabel}>{labels.treeHash}</span>
          <span data-testid="portable-skill-tree-hash" className={styles.mono}>
            {item.treeHash ?? labels.missing}
          </span>
        </div>
        <div>
          <span className={styles.metaLabel}>{labels.origin}</span>
          <span data-testid="portable-skill-origin" data-origin={item.sourceOrigin}>
            {item.sourceOrigin}
          </span>
        </div>
        <div>
          <span className={styles.metaLabel}>{labels.invocation}</span>
          <span data-testid="portable-skill-invocation" className={styles.mono}>
            {item.nativeId}
          </span>
        </div>
        <div>
          <span className={styles.metaLabel}>{labels.sourcePath}</span>
          <span data-testid="portable-skill-source-path" className={styles.mono}>
            {item.sourcePath ?? labels.missing}
          </span>
        </div>
        {item.description ? (
          <div>
            <span className={styles.metaLabel}>{labels.description}</span>
            <span data-testid="portable-skill-description">{item.description}</span>
          </div>
        ) : null}
        {item.parentPluginInventoryItemId ? (
          <div data-testid="portable-skill-parent-plugin">
            <span className={styles.metaLabel}>{labels.parentPlugin}</span>
            <span className={styles.mono}>{item.parentPluginInventoryItemId}</span>
          </div>
        ) : null}
      </div>
    </section>
  );
}

SkillDetails.displayName = 'SkillDetails';
