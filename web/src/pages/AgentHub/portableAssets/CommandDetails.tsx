/**
 * CommandDetails — Command 资产专属详情。
 *
 * Business Logic（为什么需要这个组件）:
 *   Command 需展示 native id、source file、invocation 与兼容性诊断。
 *
 * Code Logic（这个组件做什么）:
 *   pure props 渲染；不 import @/api/*。
 */

import type { PortableInventoryItemDto } from '@/lib/types/portableInventory';
import styles from '../AgentHub.module.css';

export interface CommandDetailsProps {
  item: PortableInventoryItemDto;
  labels: {
    nativeId: string;
    sourceFile: string;
    invocation: string;
    compatibility: string;
    missing: string;
    none: string;
  };
}

/**
 * Business Logic: Command 详情事实面。
 * Code Logic: warnings 作为兼容性诊断诚实展示。
 */
export function CommandDetails({ item, labels }: CommandDetailsProps) {
  return (
    <section className={styles.drawerSection} data-testid="portable-command-details">
      <div className={styles.metaBlock}>
        <div>
          <span className={styles.metaLabel}>{labels.nativeId}</span>
          <span data-testid="portable-command-native-id" className={styles.mono}>
            {item.nativeId}
          </span>
        </div>
        <div>
          <span className={styles.metaLabel}>{labels.sourceFile}</span>
          <span data-testid="portable-command-source-file" className={styles.mono}>
            {item.sourcePath ?? labels.missing}
          </span>
        </div>
        <div>
          <span className={styles.metaLabel}>{labels.invocation}</span>
          <span data-testid="portable-command-invocation" className={styles.mono}>
            {item.nativeId}
          </span>
        </div>
      </div>
      <div className={styles.drawerSection}>
        <span className={styles.metaLabel}>{labels.compatibility}</span>
        <ul className={styles.partialList} data-testid="portable-command-compatibility">
          {item.warnings.length === 0 ? (
            <li>{labels.none}</li>
          ) : (
            item.warnings.map((warning) => <li key={warning}>{warning}</li>)
          )}
        </ul>
      </div>
    </section>
  );
}

CommandDetails.displayName = 'CommandDetails';
