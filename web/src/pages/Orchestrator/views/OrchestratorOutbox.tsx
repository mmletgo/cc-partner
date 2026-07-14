/**
 * Orchestrator pending remote outbox 视图
 *
 * Business Logic（为什么需要这个组件）:
 *   远端离线创建的待发送项需要单独展示设备、状态与失败错误，且仅 failed 行提供 Retry/Discard。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 pendingRemoteItems 列表与 failed-only 动作按钮；动作回调由 controller 注入；无 API import。
 */
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Pill } from '@/components/primitives';
import type { OrchestratorRemoteOutboxItem } from '@/lib/types';
import {
  PENDING_REMOTE_STATUS_LABEL_KEYS,
  pendingRemoteStatusTone,
} from '../orchestratorViewHelpers';
import styles from '../Orchestrator.module.css';

/**
 * Business Logic（为什么需要这个类型）:
 *   Outbox 区只展示 pending remote 项与失败动作，状态机仍归 controller。
 *
 * Code Logic（这个类型做什么）:
 *   描述 pending 列表、focus 高亮、busy id 与 retry/discard 回调。
 */
export interface OrchestratorOutboxProps {
  pendingRemoteItems: OrchestratorRemoteOutboxItem[];
  focusedOutboxId: string | null;
  outboxActionId: string | null;
  onRetry: (outboxId: string) => void;
  onDiscard: (outboxId: string) => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   有 pending remote 时队列下方需要单独区块，避免与真实 workflow 任务混在一起。
 *
 * Code Logic（这个函数做什么）:
 *   列表为空返回 null；否则渲染 pending 区，仅 item.status === 'failed' 显示 Retry/Discard。
 */
export function OrchestratorOutbox(props: OrchestratorOutboxProps): JSX.Element | null {
  const { pendingRemoteItems, focusedOutboxId, outboxActionId, onRetry, onDiscard } = props;
  const { t } = useTranslation(['orchestrator']);

  if (pendingRemoteItems.length === 0) return null;

  return (
    <section className={styles.group}>
      <div className={styles.groupHeader}>
        <span>{t('orchestrator:pending.title')}</span>
        <Pill tone="warn">{pendingRemoteItems.length}</Pill>
      </div>
      <div className={styles.taskList}>
        {pendingRemoteItems.map((item) => (
          <div
            className={styles.pendingTask}
            key={item.id}
            data-focused={focusedOutboxId === item.id || undefined}
            data-testid={
              focusedOutboxId === item.id ? `orchestrator-outbox-focused-${item.id}` : undefined
            }
          >
            <div className={styles.pendingTaskHeader}>
              <span className={styles.taskTitle}>{item.deviceName}</span>
              <Pill tone={pendingRemoteStatusTone(item.status)}>
                {t(PENDING_REMOTE_STATUS_LABEL_KEYS[item.status])}
              </Pill>
            </div>
            <span className={styles.taskMeta}>
              {t('orchestrator:pending.remoteProjectPath', {
                path: item.remoteProjectPath,
              })}
            </span>
            {item.lastError ? (
              <p className={styles.pendingError}>
                {t('orchestrator:pending.lastError', { error: item.lastError })}
              </p>
            ) : null}
            {item.status === 'failed' ? (
              <div className={styles.pendingTaskActions}>
                <Button
                  variant="secondary"
                  size="sm"
                  loading={outboxActionId === item.id}
                  disabled={outboxActionId !== null && outboxActionId !== item.id}
                  onClick={() => {
                    void onRetry(item.id);
                  }}
                >
                  {t('orchestrator:pending.retry')}
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  loading={outboxActionId === item.id}
                  disabled={outboxActionId !== null && outboxActionId !== item.id}
                  onClick={() => {
                    void onDiscard(item.id);
                  }}
                >
                  {t('orchestrator:pending.discard')}
                </Button>
              </div>
            ) : null}
          </div>
        ))}
      </div>
    </section>
  );
}
