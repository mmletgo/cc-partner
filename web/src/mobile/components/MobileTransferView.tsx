/**
 * 移动端文件传输纯视图。
 *
 * Business Logic（为什么需要这个组件）:
 *   手机需要选本机/对端、选文件、发送并管理任务；不得直连 HTTP/API。
 *
 * Code Logic（这个组件做什么）:
 *   只消费 view model；复用 TransferItem 与 groupTransferTasks 动作矩阵。
 */

import type { ChangeEvent, ReactElement } from 'react';
import { useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, ProgressBar } from '@/components/primitives';
import { TransferItem } from '@/components/domain';
import {
  canOpenRevealTransfer,
  isLogicalTransferRecoveryLocked,
  isTransferResumable,
  isTransferRetryable,
} from '@/pages/Transfer/transferHistory';
import type { TransferTask } from '@/lib/types';
import type { MobileTransferViewModel } from '../controllers/useMobileTransferController';
import workbenchStyles from '../MobileWorkbench.module.css';
import styles from './MobileTransferPanel.module.css';

export type MobileTransferViewProps = MobileTransferViewModel;

/**
 * Business Logic（为什么需要这个组件）:
 *   传输面板的设备/文件/任务区必须与桌面动作矩阵对齐，但不能引入 Tauri Open/Reveal。
 *
 * Code Logic（这个组件做什么）:
 *   渲染选择器、单文件 input、本地上传进度与分区 TransferItem 列表。
 */
export function MobileTransferView(props: MobileTransferViewProps): ReactElement {
  const { t } = useTranslation(['transfer', 'common', 'workbench']);
  const fileInputRef = useRef<HTMLInputElement | null>(null);

  /**
   * Business Logic（为什么需要这个函数）:
   *   浏览器 input 只提供 FileList，页面只取首文件。
   *
   * Code Logic（这个函数做什么）:
   *   把 files[0] 交给 controller。
   */
  const handleFileInputChange = (event: ChangeEvent<HTMLInputElement>): void => {
    const file = event.target.files?.[0] ?? null;
    props.onFileChosen(file);
    event.target.value = '';
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   分区列表需要按合法动作矩阵组装 TransferItem。
   *
   * Code Logic（这个函数做什么）:
   *   pending/transferring → cancel；failed → resume 或 retry；
   *   receive completed → download（不传 open/reveal）。
   */
  const renderTaskRow = (task: TransferTask): ReactElement => {
    const reconciling = props.reconcilingIds.has(task.id);
    const recoveryLocked = isLogicalTransferRecoveryLocked(
      task,
      props.tasks,
      props.reconcilingIds,
    );
    const canCancel = task.status === 'pending' || task.status === 'transferring';
    const peerSupportsResume = props.peerSupportsResume(task);
    const canResume =
      !reconciling && !recoveryLocked && isTransferResumable(task, peerSupportsResume);
    const canRetry =
      !reconciling && !recoveryLocked && isTransferRetryable(task, peerSupportsResume);
    const canDownload = !reconciling && canOpenRevealTransfer(task);
    const actionError = props.taskActionErrors[task.id];

    return (
      <li key={task.id} className={styles.taskRow}>
        <TransferItem
          task={{
            id: task.id,
            fileName: task.fileName,
            fileSize: task.fileSize,
            direction: task.direction,
            status: task.status,
            progress: task.progress,
            peerDevice: task.peerDeviceName,
            speed: task.speed,
            errorMessage: task.failure?.message ?? task.errorMessage,
            phase: task.phase,
            reconciling,
          }}
          onCancel={canCancel ? () => props.onCancel(task.id) : undefined}
          onResume={canResume ? () => props.onResume(task.id) : undefined}
          onRetry={canRetry ? () => props.onRetry(task.id) : undefined}
          onDownload={canDownload ? () => props.onDownload(task) : undefined}
          cancelling={props.cancellingIds.has(task.id)}
        />
        {actionError ? (
          <p className={workbenchStyles.panelError} role="alert">
            {actionError}
          </p>
        ) : null}
      </li>
    );
  };

  const localProgress =
    props.localUpload && props.localUpload.fileSize > 0
      ? props.localUpload.uploadedBytes / props.localUpload.fileSize
      : 0;

  return (
    <section className={workbenchStyles.panel} aria-labelledby="mobile-transfer-panel-title">
      <div className={workbenchStyles.panelHeader}>
        <h1 id="mobile-transfer-panel-title">{t('transfer:title')}</h1>
        <p className={workbenchStyles.panelKicker}>{t('transfer:eyebrow')}</p>
      </div>

      <div className={workbenchStyles.mobileForm}>
        <label className={workbenchStyles.mobileField}>
          <span>{t('transfer:fieldLabel')}</span>
          <select
            className={workbenchStyles.mobileSelect}
            value={props.selectedDeviceId}
            onChange={(event) => {
              props.onDeviceChange(event.target.value);
            }}
            aria-label={t('transfer:selectDevice')}
            disabled={props.devicesState === 'loading' && props.devices.length === 0}
          >
            {props.devicesState === 'loading' && props.devices.length === 0 ? (
              <option value="">{t('transfer:loading')}</option>
            ) : props.devices.length === 0 ? (
              <option value="">{t('transfer:noDevices')}</option>
            ) : (
              props.devices.map((device) => (
                <option key={device.id} value={device.id}>
                  {device.isSelf
                    ? `${t('transfer:thisComputer')} · ${device.name}`
                    : device.name}
                </option>
              ))
            )}
          </select>
        </label>

        <div className={styles.fileRow}>
          <input
            ref={fileInputRef}
            className={styles.fileInput}
            type="file"
            onChange={handleFileInputChange}
            disabled={props.sending}
            aria-label={t('transfer:pickFile')}
          />
          <Button
            variant="secondary"
            size="sm"
            disabled={props.sending}
            onClick={() => {
              fileInputRef.current?.click();
            }}
          >
            {t('transfer:browse')}
          </Button>
          <p className={styles.fileName}>
            {props.selectedFileName
              ? t('transfer:dropTitlePicked', { file: props.selectedFileName })
              : t('transfer:pickFile')}
          </p>
        </div>

        <Button
          variant="primary"
          size="md"
          disabled={!props.canSend}
          loading={props.sending}
          aria-busy={props.sending || undefined}
          onClick={props.onSend}
        >
          {props.selectedFileName
            ? t('transfer:sendFile', { file: props.selectedFileName })
            : t('transfer:pickFile')}
        </Button>
        <p className={styles.hint}>{t('transfer:chunkHint')}</p>
      </div>

      {props.localUpload ? (
        <div className={styles.localUpload} role="status">
          <p className={styles.fileName}>
            {t('transfer:sendingFile', { file: props.localUpload.fileName })}
          </p>
          <p className={styles.hint}>{props.localUpload.deviceName}</p>
          <ProgressBar value={localProgress} tone="accent" />
        </div>
      ) : null}

      {props.sendError ? (
        <p className={workbenchStyles.panelError} role="alert">
          {props.sendError}
        </p>
      ) : null}

      {props.devicesState === 'error' ? (
        <p className={workbenchStyles.panelState} role="status">
          {props.devicesError}{' '}
          <Button variant="secondary" size="sm" onClick={props.onRetryDevices}>
            {t('common:action.retry')}
          </Button>
        </p>
      ) : null}

      <section className={styles.tasks} aria-label={t('transfer:tasksTitle')}>
        <h2 className={styles.tasksTitle}>{t('transfer:tasksTitle')}</h2>
        {props.tasksState === 'loading' && props.tasks.length === 0 ? (
          <p className={workbenchStyles.panelState}>{t('transfer:loading')}</p>
        ) : props.tasks.length === 0 ? (
          <div className={styles.empty}>
            <p>{t('transfer:empty')}</p>
            <p className={styles.hint}>{t('transfer:emptyHint')}</p>
          </div>
        ) : (
          <div className={styles.groupStack}>
            {props.groupedTasks.active.length > 0 ? (
              <section className={styles.taskGroup} aria-label={t('transfer:groupActive')}>
                <h3 className={styles.groupTitle}>{t('transfer:groupActive')}</h3>
                <ul className={styles.taskList}>{props.groupedTasks.active.map(renderTaskRow)}</ul>
              </section>
            ) : null}
            {props.groupedTasks.needsAttention.length > 0 ? (
              <section
                className={styles.taskGroup}
                aria-label={t('transfer:groupNeedsAttention')}
              >
                <h3 className={styles.groupTitle}>{t('transfer:groupNeedsAttention')}</h3>
                <ul className={styles.taskList}>
                  {props.groupedTasks.needsAttention.map(renderTaskRow)}
                </ul>
              </section>
            ) : null}
            {props.groupedTasks.completed.length > 0 ? (
              <section className={styles.taskGroup} aria-label={t('transfer:groupCompleted')}>
                <h3 className={styles.groupTitle}>{t('transfer:groupCompleted')}</h3>
                <ul className={styles.taskList}>
                  {props.groupedTasks.completed.map(renderTaskRow)}
                </ul>
              </section>
            ) : null}
          </div>
        )}
      </section>

      {props.tasksState === 'error' ? (
        <p className={workbenchStyles.panelState} role="status">
          {props.tasksError}{' '}
          <Button variant="secondary" size="sm" onClick={props.onRetryTasks}>
            {t('common:action.retry')}
          </Button>
        </p>
      ) : null}
    </section>
  );
}
