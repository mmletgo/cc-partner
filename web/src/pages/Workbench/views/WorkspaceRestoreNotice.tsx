/**
 * WorkspaceRestoreNotice — partial restore 单条 inline notice。
 *
 * Business Logic（为什么需要）:
 *   无法完整恢复时只显示一次可关闭摘要；完全成功静默。
 *
 * Code Logic（做什么）:
 *   role=status；可展开 bounded reason codes；无 terminal 内容/绝对路径。
 */

import { useId, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/primitives/Button';
import type { WorkspaceRestoreSummary } from '../workspaceRestore';
import { formatRestoreNotice } from '../workspaceRestore';
import styles from '../Workbench.module.css';

export interface WorkspaceRestoreNoticeProps {
  summary: WorkspaceRestoreSummary | null;
  onDismiss: () => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   partial restore 给用户一次可理解的摘要。
 *
 * Code Logic（这个函数做什么）:
 *   silent/null 不渲染；否则 status live region + 可选 reason 列表。
 */
export function WorkspaceRestoreNotice(
  props: WorkspaceRestoreNoticeProps,
): ReactElement | null {
  const { summary, onDismiss } = props;
  const { t } = useTranslation(['workbench']);
  const [expanded, setExpanded] = useState(false);
  const titleId = useId();

  if (!summary || summary.silent || summary.status === 'complete') {
    return null;
  }

  return (
    <div
      className={styles.restoreNotice}
      role="status"
      aria-labelledby={titleId}
      data-testid="workspace-restore-notice"
    >
      <div className={styles.restoreNoticeRow}>
        <p id={titleId} className={styles.restoreNoticeText}>
          {formatRestoreNotice(summary)}
        </p>
        <div className={styles.restoreNoticeActions}>
          {summary.reasons.length > 0 ? (
            <Button
              variant="ghost"
              size="sm"
              type="button"
              onClick={() => setExpanded((v) => !v)}
              aria-expanded={expanded}
            >
              {expanded
                ? t('workbench:workspaceRestore.collapseReasons')
                : t('workbench:workspaceRestore.viewReasons')}
            </Button>
          ) : null}
          <Button variant="ghost" size="sm" type="button" onClick={onDismiss}>
            {t('workbench:workspaceRestore.close')}
          </Button>
        </div>
      </div>
      {expanded ? (
        <ul className={styles.restoreNoticeReasons}>
          {summary.reasons.map((reason) => (
            <li key={reason}>{reason}</li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}
