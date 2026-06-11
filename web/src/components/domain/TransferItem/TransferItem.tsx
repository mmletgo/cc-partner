/**
 * TransferItem 业务组件
 *
 * Business Logic（为什么需要这个组件）:
 *   文件传输列表需要为每个传输任务渲染一行可视单元，展示文件名/方向/进度/对端/状态/速度，
 *   并根据状态提供对应的操作（暂停/继续/取消/重试/打开）。统一的行布局让用户
 *   在同时进行多个传输时仍能一眼看清每条任务的进展。
 *
 * Code Logic（这个组件做什么）:
 *   - 基于 Card（flat）渲染，2px 左边框 + 12% 透明背景色根据 status 变化
 *   - 左侧方向图标（Send/Download），中间文件名/对端/进度条，右侧 Pill+速度+操作按钮
 *   - 内部工具函数 formatBytes / formatSpeed 把字节数和带宽格式化为人类可读单位
 *   - 不同 status 渲染不同按钮组（transferring=Pause+X, failed=Retry, completed=Open）
 */

import { memo, useCallback } from 'react';
import type { CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Pill, ProgressBar } from '@/components/primitives';
import { CheckIcon, DownloadIcon, PauseIcon, PlayIcon, SendIcon, XIcon } from '@/lib/icons';
import styles from './TransferItem.module.css';

export type TransferDirection = 'send' | 'receive';
export type TransferStatus = 'pending' | 'transferring' | 'completed' | 'failed' | 'cancelled';

/** 传输任务数据模型 */
export interface TransferItemTask {
  id: string;
  fileName: string;
  fileSize: number;
  direction: TransferDirection;
  status: TransferStatus;
  /** 0-1 */
  progress: number;
  peerDevice?: string;
  /** 字节/秒 */
  speed?: number;
  errorMessage?: string;
}

export interface TransferItemProps {
  task: TransferItemTask;
  onPause?: () => void;
  onResume?: () => void;
  onCancel?: () => void;
  onRetry?: () => void;
  onOpen?: () => void;
  className?: string;
  style?: CSSProperties;
}

const STATUS_TONE = {
  pending: 'neutral',
  transferring: 'accent',
  completed: 'success',
  failed: 'danger',
  cancelled: 'warn',
} as const;

const STATUS_BG = {
  pending: 'var(--surface-warm)',
  transferring: 'var(--accent-soft)',
  completed: 'color-mix(in oklab, var(--success) 12%, transparent)',
  failed: 'var(--danger-soft)',
  cancelled: 'color-mix(in oklab, var(--warn) 14%, transparent)',
} as const;

const STATUS_BORDER = {
  pending: 'var(--border)',
  transferring: 'var(--accent)',
  completed: 'var(--success)',
  failed: 'var(--danger)',
  cancelled: 'var(--warn)',
} as const;

/**
 * 把字节数格式化为人类可读字符串（1.5 MB / 230 KB / 1.0 GB）
 */
function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '0 B';
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = bytes / 1024;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  const decimals = value >= 100 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(decimals)} ${units[unitIndex]}`;
}

/**
 * 把字节/秒格式化为带宽字符串（1.2 MB/s）
 */
function formatSpeed(bytesPerSec: number): string {
  if (!Number.isFinite(bytesPerSec) || bytesPerSec <= 0) return '0 B/s';
  return `${formatBytes(bytesPerSec)}/s`;
}

/**
 * 渲染文件传输列表项
 */
function TransferItemInner({ task, onPause, onResume, onCancel, onRetry, onOpen, className, style }: TransferItemProps) {
  const { t } = useTranslation(['common']);
  const tone = STATUS_TONE[task.status];
  const label = t(`common:status.transfer.${task.status}`);
  const bg = STATUS_BG[task.status];
  const border = STATUS_BORDER[task.status];
  // ProgressBar 不支持 neutral，未传输/未开始的也用 accent 表示「进行中」色
  const progressTone: 'accent' | 'success' | 'warn' | 'danger' =
    tone === 'neutral' ? 'accent' : tone;

  const handlePause = useCallback(() => onPause?.(), [onPause]);
  const handleResume = useCallback(() => onResume?.(), [onResume]);
  const handleCancel = useCallback(() => onCancel?.(), [onCancel]);
  const handleRetry = useCallback(() => onRetry?.(), [onRetry]);
  const handleOpen = useCallback(() => onOpen?.(), [onOpen]);

  const isProgressVisible = task.status === 'transferring' || task.status === 'pending';
  const transferredBytes = Math.max(0, Math.min(1, task.progress)) * task.fileSize;

  const cardClasses = [styles.card, className].filter(Boolean).join(' ');

  const cardStyle: CSSProperties = {
    backgroundColor: bg,
    borderLeft: `2px solid ${border}`,
    ...style,
  };

  const DirectionIcon = task.direction === 'send' ? SendIcon : DownloadIcon;

  return (
    <Card variant="flat" className={cardClasses} style={cardStyle}>
      <Card.Body padding="md" className={styles.body}>
        <div className={styles.row}>
          <div className={styles.left}>
            <span className={styles.directionIcon} aria-hidden="true">
              <DirectionIcon />
            </span>
          </div>

          <div className={styles.middle}>
            <div className={styles.fileName} title={task.fileName}>
              {task.fileName}
            </div>
            <div className={styles.peer}>{task.peerDevice ?? t(`common:direction.${task.direction}`)}</div>
            {isProgressVisible ? (
              <div className={styles.progressRow}>
                <ProgressBar value={task.progress} tone={progressTone} className={styles.progress} />
                <span className={styles.sizeText}>
                  {formatBytes(transferredBytes)} / {formatBytes(task.fileSize)}
                </span>
              </div>
            ) : (
              <div className={styles.sizeRow}>
                <span className={styles.sizeText}>{formatBytes(task.fileSize)}</span>
                {task.errorMessage ? <span className={styles.errorText}>{task.errorMessage}</span> : null}
              </div>
            )}
          </div>

          <div className={styles.right}>
            <Pill tone={tone} className={styles.statusPill}>
              {label}
            </Pill>
            {task.status === 'transferring' && task.speed !== undefined ? (
              <span className={styles.speed}>{formatSpeed(task.speed)}</span>
            ) : null}
            <div className={styles.actions}>
              {task.status === 'transferring' ? (
                <>
                  <Button
                    variant="ghost"
                    size="sm"
                    icon={<PauseIcon />}
                    onClick={handlePause}
                    aria-label={t('common:action.pause')}
                    title={t('common:action.pause')}
                  />
                  <Button
                    variant="danger"
                    size="sm"
                    icon={<XIcon />}
                    onClick={handleCancel}
                    aria-label={t('common:action.cancel')}
                    title={t('common:action.cancel')}
                  />
                </>
              ) : null}
              {task.status === 'pending' ? (
                <Button
                  variant="ghost"
                  size="sm"
                  icon={<XIcon />}
                  onClick={handleCancel}
                  aria-label={t('common:action.cancel')}
                  title={t('common:action.cancel')}
                />
              ) : null}
              {task.status === 'failed' ? (
                <Button
                  variant="secondary"
                  size="sm"
                  icon={<PlayIcon />}
                  onClick={handleRetry}
                  aria-label={t('common:action.retry')}
                  title={t('common:action.retry')}
                >
                  {t('common:action.retry')}
                </Button>
              ) : null}
              {task.status === 'cancelled' && onResume ? (
                <Button
                  variant="secondary"
                  size="sm"
                  icon={<PlayIcon />}
                  onClick={handleResume}
                  aria-label={t('common:action.continue')}
                  title={t('common:action.continue')}
                >
                  {t('common:action.continue')}
                </Button>
              ) : null}
              {task.status === 'completed' ? (
                <Button
                  variant="ghost"
                  size="sm"
                  icon={<CheckIcon />}
                  onClick={handleOpen}
                  aria-label={t('common:action.open')}
                  title={t('common:action.open')}
                />
              ) : null}
            </div>
          </div>
        </div>
      </Card.Body>
    </Card>
  );
}

export const TransferItem = memo(TransferItemInner);
TransferItem.displayName = 'TransferItem';
