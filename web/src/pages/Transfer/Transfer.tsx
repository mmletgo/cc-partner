/**
 * Transfer 页面 - 局域网文件传输
 *
 * Business Logic（为什么需要这个页面）:
 *   用户需要在一个屏幕里完成：选目标设备、选本机文件路径、发送、监控进度与取消。
 *   路径必须来自原生 dialog/drag，展示 basename，完整路径仅内存保存并透传后端。
 *
 * Code Logic（这个页面做什么）:
 *   - 设备 5s / 任务 3s visibility-aware polling；刷新失败保留已有数组
 *   - 选中文件存 {path,name}；Enter/Space/点击走 pickTransferFile；native drop 只取首路径
 *   - handleSendClick await transferApi.send 后 await runTasksNow；track sending/sendError
 *   - pending/transferring 仅传 onCancel；cancellingIds + taskActionErrors 行级反馈
 */

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ChangeEvent, KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Pill } from '@/components/primitives';
import { TransferItem } from '@/components/domain';
import { devicesApi } from '@/api/devices';
import { transferApi } from '@/api/transfer';
import { useVisibilityPolling } from '@/hooks/useVisibilityPolling';
import type { Device, TransferTask } from '@/lib/types';
import { SendIcon, UploadIcon } from '@/lib/icons';
import { pickTransferFile, subscribeTransferFileDrops } from './transferFileSelection';
import styles from './Transfer.module.css';

const TASK_REFRESH_INTERVAL_MS = 3000;
const DEVICE_REFRESH_INTERVAL_MS = 5000;

type LoadState = 'loading' | 'success' | 'error';

/**
 * Business Logic（为什么需要这个类型）:
 *   发送只需要绝对路径与展示用 basename，不能用浏览器 File（无可靠路径）。
 *
 * Code Logic（这个类型做什么）:
 *   path 为不透明 UTF-8 绝对路径；name 为 basename 展示。
 */
interface SelectedTransferFile {
  path: string;
  name: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   UI 只能展示 basename，不得暴露完整绝对路径。
 *
 * Code Logic（这个函数做什么）:
 *   按最后一次 / 或 \\ 切分路径，得到文件名；路径本身不做改写。
 */
function basenameFromPath(path: string): string {
  const normalized = path.replace(/\\/g, '/');
  const parts = normalized.split('/');
  const last = parts[parts.length - 1] ?? '';
  return last.length > 0 ? last : path;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   错误对象可能是 Error 或未知 reject，需要稳定可读文案。
 *
 * Code Logic（这个函数做什么）:
 *   Error 取 message，其余 String()。
 */
function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * Business Logic（为什么需要这个组件）:
 *   局域网文件传输主视图：设备选择、路径选择、发送、任务监控与取消。
 *
 * Code Logic（这个组件做什么）:
 *   挂载 visibility polling、native drop 订阅，管理 send/cancel 状态机并渲染列表。
 */
export function Transfer() {
  const { t } = useTranslation(['transfer', 'common']);

  // ── 设备列表（目标设备下拉数据源） ──
  const [devices, setDevices] = useState<Device[]>([]);
  const [selectedDeviceId, setSelectedDeviceId] = useState<string>('');
  const [devicesState, setDevicesState] = useState<LoadState>('loading');
  const [devicesError, setDevicesError] = useState<string | null>(null);

  // ── 任务列表 ──
  const [tasks, setTasks] = useState<TransferTask[]>([]);
  const [tasksState, setTasksState] = useState<LoadState>('loading');
  const [tasksError, setTasksError] = useState<string | null>(null);

  // ── 文件选择 / 拖拽 ──
  const [selectedFile, setSelectedFile] = useState<SelectedTransferFile | null>(null);
  const [isDragOver, setIsDragOver] = useState(false);
  const [selectionNotice, setSelectionNotice] = useState<string | null>(null);

  // ── 发送 / 取消动作状态 ──
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const [cancellingIds, setCancellingIds] = useState<ReadonlySet<string>>(() => new Set());
  const [taskActionErrors, setTaskActionErrors] = useState<Record<string, string>>({});

  /**
   * Business Logic（为什么需要这个函数）:
   *   设备下拉需要与后端发现列表对齐；刷新失败不得清空已成功数据。
   *
   * Code Logic（这个函数做什么）:
   *   调用 devicesApi.list；成功写数组；失败保留 prev 并标 error。
   */
  const loadDevices = useCallback(async () => {
    try {
      const data = await devicesApi.list();
      const next = Array.isArray(data) ? data : [];
      setDevices(next);
      if (next.length > 0) {
        setSelectedDeviceId((prev) => prev || next[0]!.id);
      }
      setDevicesState('success');
      setDevicesError(null);
    } catch (err) {
      setDevicesState('error');
      setDevicesError(t('transfer:deviceLoadFailed', { error: errorMessage(err) }));
      // 保留已有 devices，不覆盖为空
    }
  }, [t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   任务列表是发送/取消后的权威进度源；刷新失败保留旧列表。
   *
   * Code Logic（这个函数做什么）:
   *   调用 transferApi.list；成功写数组；失败保留 prev 并标 error。
   */
  const loadTasks = useCallback(async () => {
    try {
      const data = await transferApi.list();
      setTasks(Array.isArray(data) ? data : []);
      setTasksState('success');
      setTasksError(null);
    } catch (err) {
      setTasksState('error');
      setTasksError(t('transfer:taskLoadFailed', { error: errorMessage(err) }));
      // 保留已有 tasks，不覆盖为空
    }
  }, [t]);

  const { runNow: runTasksNow } = useVisibilityPolling(loadTasks, {
    intervalMs: TASK_REFRESH_INTERVAL_MS,
  });

  useVisibilityPolling(loadDevices, {
    intervalMs: DEVICE_REFRESH_INTERVAL_MS,
  });

  // ── 状态计数（按 status 分组） ──
  const statusCounts = useMemo(() => {
    return tasks.reduce(
      (acc, task) => {
        if (task.status === 'transferring' || task.status === 'pending') acc.active += 1;
        else if (task.status === 'completed') acc.completed += 1;
        else if (task.status === 'failed' || task.status === 'cancelled') acc.failed += 1;
        return acc;
      },
      { active: 0, completed: 0, failed: 0 },
    );
  }, [tasks]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   原生路径选中后需要只展示 basename，完整路径仅用于 send。
   *
   * Code Logic（这个函数做什么）:
   *   写入 selectedFile，可选清理/设置多文件 notice。
   */
  const applySelectedPath = useCallback((path: string, multi: boolean) => {
    setSelectedFile({ path, name: basenameFromPath(path) });
    setSendError(null);
    setSelectionNotice(multi ? t('transfer:firstFileOnly') : null);
  }, [t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   浏览按钮与 dropzone 键盘操作需要打开原生单文件选择器。
   *
   * Code Logic（这个函数做什么）:
   *   await pickTransferFile；取消(null)保留原选择；成功写 {path,name}。
   */
  const handlePickClick = useCallback(async () => {
    const path = await pickTransferFile();
    if (path == null) return;
    applySelectedPath(path, false);
  }, [applySelectedPath]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   dropzone 需支持键盘可达，Enter/Space 打开原生选择器。
   *
   * Code Logic（这个函数做什么）:
   *   拦截 Enter/Space，preventDefault 后触发 handlePickClick。
   */
  const handleDropzoneKeyDown = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      if (event.key !== 'Enter' && event.key !== ' ') return;
      event.preventDefault();
      void handlePickClick();
    },
    [handlePickClick],
  );

  // 挂载时订阅 native drag-drop；卸载时 unlisten
  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;

    void (async () => {
      const stop = await subscribeTransferFileDrops((paths) => {
        if (!active) return;
        const first = paths[0];
        if (!first) return;
        setIsDragOver(false);
        applySelectedPath(first, paths.length > 1);
      });
      if (!active) {
        stop();
        return;
      }
      unlisten = stop;
    })();

    return () => {
      active = false;
      unlisten?.();
    };
  }, [applySelectedPath]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户切换目标设备时更新下拉选中项。
   *
   * Code Logic（这个函数做什么）:
   *   写 selectedDeviceId。
   */
  const handleDeviceChange = useCallback((e: ChangeEvent<HTMLSelectElement>) => {
    setSelectedDeviceId(e.target.value);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户确认发送后必须真实调用 send_transfer，成功刷新任务列表。
   *
   * Code Logic（这个函数做什么）:
   *   校验 selection/device；await transferApi.send；成功清空选择并 runTasksNow；失败保留选择。
   */
  const handleSendClick = useCallback(async () => {
    if (!selectedFile || !selectedDeviceId || sending) return;
    setSending(true);
    setSendError(null);
    try {
      await transferApi.send(selectedDeviceId, selectedFile.path);
      setSelectedFile(null);
      setSelectionNotice(null);
      await runTasksNow();
    } catch (err) {
      setSendError(t('transfer:sendFailed', { error: errorMessage(err) }));
    } finally {
      setSending(false);
    }
  }, [selectedFile, selectedDeviceId, sending, runTasksNow, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户可取消 pending/transferring 任务，失败需行级提示且保留任务。
   *
   * Code Logic（这个函数做什么）:
   *   维护 cancellingIds；await cancel；成功清错误并 runTasksNow；失败写 taskActionErrors。
   */
  const handleCancelTask = useCallback(
    async (taskId: string) => {
      setCancellingIds((prev) => {
        const next = new Set(prev);
        next.add(taskId);
        return next;
      });
      setTaskActionErrors((prev) => {
        if (!(taskId in prev)) return prev;
        const next = { ...prev };
        delete next[taskId];
        return next;
      });
      try {
        await transferApi.cancel(taskId);
        await runTasksNow();
      } catch (err) {
        setTaskActionErrors((prev) => ({
          ...prev,
          [taskId]: t('transfer:cancelFailed', { error: errorMessage(err) }),
        }));
      } finally {
        setCancellingIds((prev) => {
          const next = new Set(prev);
          next.delete(taskId);
          return next;
        });
      }
    },
    [runTasksNow, t],
  );

  const dropzoneClasses = [styles.dropzone, isDragOver ? styles.dropzoneOver : '']
    .filter(Boolean)
    .join(' ');

  const pickedName = selectedFile?.name ?? null;
  const canSend = Boolean(selectedFile && selectedDeviceId) && !sending;

  return (
    <div className={styles.page}>
      {/* 页面头部 */}
      <header className={styles.pageHeader}>
        <span className={styles.eyebrow}>Transfer</span>
        <h1 className={styles.title}>{t('transfer:title')}</h1>
        <p className={styles.lead}>{t('transfer:lead')}</p>
      </header>

      {/* 发送区 */}
      <Card variant="elevated" className={styles.sendCard}>
        <div className={styles.sendTop}>
          <label className={styles.field}>
            <span className={styles.fieldLabel}>{t('transfer:fieldLabel')}</span>
            <div className={styles.selectWrap}>
              <select
                className={styles.select}
                value={selectedDeviceId}
                onChange={handleDeviceChange}
                aria-label={t('transfer:selectDevice')}
                disabled={devicesState === 'loading' && devices.length === 0}
              >
                {devicesState === 'loading' && devices.length === 0 ? (
                  <option value="">{t('transfer:loading')}</option>
                ) : devices.length === 0 ? (
                  <option value="">{t('transfer:noDevices')}</option>
                ) : (
                  devices.map((d) => (
                    <option key={d.id} value={d.id}>
                      {d.name} · {d.address}:{d.port}
                    </option>
                  ))
                )}
              </select>
              <span className={styles.selectArrow} aria-hidden="true">
                ▾
              </span>
            </div>
          </label>

          <div className={styles.pickerCol}>
            <Button
              variant="primary"
              size="md"
              icon={<SendIcon />}
              onClick={() => void handleSendClick()}
              disabled={!canSend}
              loading={sending}
              aria-busy={sending || undefined}
            >
              {pickedName
                ? t('transfer:sendFile', { file: pickedName })
                : t('transfer:pickFile')}
            </Button>
            <Button
              variant="secondary"
              size="md"
              onClick={() => void handlePickClick()}
              disabled={sending}
            >
              {t('transfer:browse')}
            </Button>
          </div>
        </div>

        <div
          className={dropzoneClasses}
          onDragOver={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setIsDragOver(true);
          }}
          onDragLeave={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setIsDragOver(false);
          }}
          onDrop={(e) => {
            // 浏览器 HTML5 drop 不提供原生绝对路径；真实路径来自 Tauri onDragDropEvent。
            e.preventDefault();
            e.stopPropagation();
            setIsDragOver(false);
          }}
          onClick={() => void handlePickClick()}
          onKeyDown={handleDropzoneKeyDown}
          role="button"
          tabIndex={0}
          aria-label={t('transfer:dropAria')}
        >
          <span className={styles.dropIcon} aria-hidden="true">
            <UploadIcon size={20} />
          </span>
          <p className={styles.dropTitle}>
            {pickedName
              ? t('transfer:dropTitlePicked', { file: pickedName })
              : t('transfer:dropTitleEmpty')}
          </p>
          <p className={styles.dropHint}>{t('transfer:chunkHint')}</p>
        </div>

        {selectionNotice ? (
          <p className={styles.notice} role="alert">
            {selectionNotice}
          </p>
        ) : null}

        {sendError ? (
          <p className={styles.alert} role="alert">
            {sendError}
          </p>
        ) : null}

        {devicesState === 'error' ? (
          <p className={styles.notice} role="status">
            {devicesError}{' '}
            <Button variant="secondary" size="sm" onClick={() => void loadDevices()}>
              {t('common:action.retry')}
            </Button>
          </p>
        ) : null}
      </Card>

      {/* 任务列表 */}
      <section className={styles.tasksSection}>
        <div className={styles.sectionHead}>
          <h2 className={styles.sectionTitle}>
            {t('transfer:tasksTitle')}{' '}
            <span className={styles.sectionCount}>({tasks.length})</span>
          </h2>
          <div className={styles.statusPills}>
            <Pill tone="accent" dot>
              {t('transfer:active', { n: statusCounts.active })}
            </Pill>
            <Pill tone="success">
              {t('transfer:completed', { n: statusCounts.completed })}
            </Pill>
            <Pill tone="danger">{t('transfer:failed', { n: statusCounts.failed })}</Pill>
          </div>
        </div>

        {tasksState === 'loading' && tasks.length === 0 ? (
          <TaskListSkeleton />
        ) : tasks.length === 0 ? (
          <div className={styles.empty}>
            <p>{t('transfer:empty')}</p>
            <p className={styles.emptyHint}>{t('transfer:emptyHint')}</p>
          </div>
        ) : (
          <ul className={styles.taskList}>
            {tasks.map((task) => {
              const canCancel = task.status === 'pending' || task.status === 'transferring';
              const actionError = taskActionErrors[task.id];
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
                      errorMessage: task.errorMessage,
                    }}
                    onCancel={canCancel ? () => void handleCancelTask(task.id) : undefined}
                    cancelling={cancellingIds.has(task.id)}
                  />
                  {actionError ? (
                    <p className={styles.rowAlert} role="alert">
                      {actionError}
                    </p>
                  ) : null}
                </li>
              );
            })}
          </ul>
        )}

        {tasksState === 'error' ? (
          <p className={styles.notice} role="status">
            {tasksError}{' '}
            <Button variant="secondary" size="sm" onClick={() => void loadTasks()}>
              {t('common:action.retry')}
            </Button>
          </p>
        ) : null}
      </section>
    </div>
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   首屏任务加载时需要骨架屏，避免空白闪烁。
 *
 * Code Logic（这个函数做什么）:
 *   渲染三条静态骨架行，aria-busy=true。
 */
function TaskListSkeleton() {
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
