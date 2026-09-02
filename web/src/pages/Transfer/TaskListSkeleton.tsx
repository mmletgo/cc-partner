import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';
import styles from './Transfer.module.css';

/**
 * Business Logic（为什么需要这个函数）:
 *   首屏任务加载时需要骨架屏，避免空白闪烁。
 *
 * Code Logic（这个函数做什么）:
 *   渲染三条静态骨架行，aria-busy=true。
 */
export function TaskListSkeleton(): JSX.Element {
  const { t } = useTranslation(['transfer']);
  return (
    <ul className={styles.taskList} aria-busy="true" aria-label={t('transfer:skeletonAria')}>
      {[0, 1, 2].map((i) => (
        <li key={i} className={styles.skeletonRow}>
          <span
            className={styles.skeletonBlock}
            style={{ width: 32, height: 32, borderRadius: 'var(--radius-md)' }}
          />
          <span className={styles.skeletonLines}>
            <span className={styles.skeletonBlock} style={{ width: '40%', height: 12 }} />
            <span className={styles.skeletonBlock} style={{ width: '60%', height: 10 }} />
          </span>
        </li>
      ))}
    </ul>
  );
}
