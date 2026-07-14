import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import {
  MOBILE_AUTOMATION_PENDING_STATUS_LABEL_KEYS,
  pendingRemoteTaskTitle,
  type MobileAutomationOutboxProps,
} from '../controllers/useMobileAutomationController';
import styles from '../MobileWorkbench.module.css';

/**
 * MobileAutomationOutbox（移动端 pending remote outbox 列表）
 *
 * Business Logic（为什么需要这个组件）:
 *   远端离线创建会落入本机 outbox；手机端需要单独展示 pending 区并仅对 failed 提供 Retry/Discard。
 *
 * Code Logic（这个组件做什么）:
 *   纯展示：pendingRemoteItems.map 渲染 outbox 行；failed 时调用 onRetry/onDiscard。
 *   不导入 transport/API。
 */
export function MobileAutomationOutbox({
  pendingRemoteItems,
  focusedOutboxId,
  outboxActionId,
  onRetry,
  onDiscard,
}: MobileAutomationOutboxProps): ReactElement | null {
  const { t } = useTranslation(['workbench', 'orchestrator']);

  if (pendingRemoteItems.length === 0) {
    return null;
  }

  return (
    <section className={styles.mobileAutomationGroup}>
      <div className={styles.mobileAutomationGroupHeader}>
        <span>{t('workbench:mobile.automationPanel.pendingTitle')}</span>
        <span className={styles.mobileBadge}>
          {t('workbench:mobile.automationPanel.taskCount', {
            count: pendingRemoteItems.length,
          })}
        </span>
      </div>
      <div className={styles.mobileList}>
        {pendingRemoteItems.map((item) => (
          <article
            key={item.id}
            className={`${styles.mobileListItem} ${
              focusedOutboxId === item.id ? styles.mobileListItemActive : ''
            }`}
            data-attention-outbox={focusedOutboxId === item.id ? 'true' : undefined}
          >
            <div className={styles.mobileListTitleRow}>
              <strong className={styles.mobileListTitle}>
                {pendingRemoteTaskTitle(item)}
              </strong>
              <span className={`${styles.mobileBadge} ${styles.mobileBadgeAccent}`}>
                {t(MOBILE_AUTOMATION_PENDING_STATUS_LABEL_KEYS[item.status])}
              </span>
            </div>
            <div className={styles.automationTaskBody}>
              <p>
                {t('workbench:mobile.automationPanel.pendingDevice', {
                  deviceName: item.deviceName,
                })}
              </p>
              <p>
                {item.lastError
                  ? t('workbench:mobile.automationPanel.pendingError', {
                      error: item.lastError,
                    })
                  : t('workbench:mobile.automationPanel.remoteProjectPath', {
                      path: item.remoteProjectPath,
                    })}
              </p>
            </div>
            <div className={styles.mobileBadgeRow}>
              <span className={styles.mobileBadge}>
                {t('workbench:mobile.automationPanel.origin.pending')}
              </span>
            </div>
            {item.status === 'failed' ? (
              <div className={styles.mobileBadgeRow}>
                <button
                  type="button"
                  className={styles.secondaryButton}
                  disabled={outboxActionId !== null}
                  onClick={() => {
                    onRetry(item.id);
                  }}
                >
                  {t('workbench:mobile.automationPanel.pendingRetry')}
                </button>
                <button
                  type="button"
                  className={styles.secondaryButton}
                  disabled={outboxActionId !== null}
                  onClick={() => {
                    onDiscard(item.id);
                  }}
                >
                  {t('workbench:mobile.automationPanel.pendingDiscard')}
                </button>
              </div>
            ) : null}
          </article>
        ))}
      </div>
    </section>
  );
}

export type { MobileAutomationOutboxProps };
