/**
 * TransferItem 业务组件
 *
 * Business Logic（为什么需要这个组件）:
 *   文件传输列表需要为每个传输任务渲染一行可视单元，展示文件名/方向/进度/对端/状态/速度/阶段，
 *   并根据后端真实支持的动作提供操作按钮。无回调的动作不得渲染；对账中不提供重复动作。
 *
 * Code Logic（这个组件做什么）:
 *   - 基于 Card（flat）渲染，2px 左边框 + 状态色背景
 *   - 左侧方向图标，中间文件名/对端/进度条/失败信息，右侧 Pill+速度+操作按钮
 *   - 每个动作按钮仅在对应回调存在时渲染；reconciling 时只显示“正在确认结果”
 */

import { memo, useCallback } from 'react';
import type { CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Pill, ProgressBar } from '@/components/primitives';
import {
  CheckIcon,
  DownloadIcon,
  FolderIcon,
  PauseIcon,
  PlayIcon,
  SendIcon,
  XIcon,
} from '@/lib/icons';
import type { TransferPhase } from '@/lib/types';
import { registerTransferLocale } from '@/i18n/registerTransferLocale';
import styles from './TransferItem.module.css';

registerTransferLocale();

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
  /** 细粒度阶段（可选展示） */
  phase?: TransferPhase;
  /**
   * 结果不确定、正在按 clientOperationId 对账时为 true。
   * 此时不得再渲染 retry/resume/send 类动作。
   */
  reconciling?: boolean;
}

export interface TransferItemProps {
  task: TransferItemTask;
  onPause?: () => void;
  /** 失败可续传：显示「继续传输」 */
  onResume?: () => void;
  onCancel?: () => void;
  /** 失败可重传 / 取消后重发：显示「重新传输」 */
  onRetry?: () => void;
  onOpen?: () => void;
  /** 在文件夹中显示（仅 same-device receive completed） */
  onReveal?: () => void;
  /** 下载已完成任务（移动端 Receive completed；桌面不传） */
  onDownload?: () => void;
  /** 取消进行中时禁用取消按钮并标 aria-busy */
  cancelling?: boolean;
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
 * Business Logic（为什么需要这个函数）:
 *   传输列表需要把字节数显示为人类可读单位，便于判断文件规模与进度。
 *
 * Code Logic（这个函数做什么）:
 *   把非负字节数格式化为 B/KB/MB/GB/TB 字符串。
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
 * Business Logic（为什么需要这个函数）:
 *   transferring 状态需要显示实时带宽，帮助用户判断链路是否正常。
 *
 * Code Logic（这个函数做什么）:
 *   把字节/秒格式化为「单位/s」字符串。
 */
function formatSpeed(bytesPerSec: number): string {
  if (!Number.isFinite(bytesPerSec) || bytesPerSec <= 0) return '0 B/s';
  return `${formatBytes(bytesPerSec)}/s`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   列表行需要根据任务状态与可用回调渲染安全的操作入口。
 *
 * Code Logic（这个函数做什么）:
 *   渲染文件名/进度/阶段/状态；reconciling 仅提示对账；
 *   其余仅在回调存在时显示 pause/resume/cancel/retry/open/reveal/download。
 */
function TransferItemInner({
  task,
  onPause,
  onResume,
  onCancel,
  onRetry,
  onOpen,
  onReveal,
  onDownload,
  cancelling = false,
  className,
  style,
}: TransferItemProps) {
  const { t } = useTranslation(['common', 'transfer']);
  const reconciling = Boolean(task.reconciling);
  const tone = reconciling ? 'warn' : STATUS_TONE[task.status];
  const bg = reconciling
    ? 'color-mix(in oklab, var(--warn) 14%, transparent)'
    : STATUS_BG[task.status];
  const border = reconciling ? 'var(--warn)' : STATUS_BORDER[task.status];
  // ProgressBar 不支持 neutral，未传输/未开始的也用 accent 表示「进行中」色
  const progressTone: 'accent' | 'success' | 'warn' | 'danger' =
    tone === 'neutral' ? 'accent' : tone;

  const statusLabel = reconciling
    ? t('transfer:reconciling')
    : task.phase
      ? t(`transfer:phase.${task.phase}`)
      : t(`common:status.transfer.${task.status}`);

  const handlePause = useCallback(() => onPause?.(), [onPause]);
  const handleResume = useCallback(() => onResume?.(), [onResume]);
  const handleCancel = useCallback(() => onCancel?.(), [onCancel]);
  const handleRetry = useCallback(() => onRetry?.(), [onRetry]);
  const handleOpen = useCallback(() => onOpen?.(), [onOpen]);
  const handleReveal = useCallback(() => onReveal?.(), [onReveal]);
  const handleDownload = useCallback(() => onDownload?.(), [onDownload]);

  const isProgressVisible =
    !reconciling && (task.status === 'transferring' || task.status === 'pending');
  const transferredBytes = Math.max(0, Math.min(1, task.progress)) * task.fileSize;

  const cardClasses = [styles.card, className].filter(Boolean).join(' ');

  const cardStyle: CSSProperties = {
    backgroundColor: bg,
    borderLeft: `2px solid ${border}`,
    ...style,
  };

  const DirectionIcon = task.direction === 'send' ? SendIcon : DownloadIcon;
  const showActions = !reconciling;

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
            <div className={styles.peer}>
              {task.peerDevice ?? t(`common:direction.${task.direction}`)}
            </div>
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
                {task.errorMessage ? (
                  <span className={styles.errorText}>{task.errorMessage}</span>
                ) : null}
              </div>
            )}
            {reconciling ? (
              <p className={styles.reconcilingText} role="status">
                {t('transfer:reconciling')}
              </p>
            ) : null}
          </div>

          <div className={styles.right}>
            <Pill tone={tone} className={styles.statusPill}>
              {statusLabel}
            </Pill>
            {task.status === 'transferring' && task.speed !== undefined && !reconciling ? (
              <span className={styles.speed}>{formatSpeed(task.speed)}</span>
            ) : null}
            {showActions ? (
              <div className={styles.actions}>
                {task.status === 'transferring' && onPause ? (
                  <Button
                    variant="ghost"
                    size="sm"
                    icon={<PauseIcon />}
                    onClick={handlePause}
                    aria-label={t('common:action.pause')}
                    title={t('common:action.pause')}
                  />
                ) : null}
                {(task.status === 'transferring' || task.status === 'pending') && onCancel ? (
                  <Button
                    variant={task.status === 'transferring' ? 'danger' : 'ghost'}
                    size="sm"
                    icon={<XIcon />}
                    onClick={handleCancel}
                    disabled={cancelling}
                    aria-busy={cancelling || undefined}
                    aria-label={t('common:action.cancel')}
                    title={t('common:action.cancel')}
                  />
                ) : null}
                {task.status === 'failed' && onResume ? (
                  <Button
                    variant="secondary"
                    size="sm"
                    icon={<PlayIcon />}
                    onClick={handleResume}
                    aria-label={t('transfer:resumeTransfer')}
                    title={t('transfer:resumeTransfer')}
                  >
                    {t('transfer:resumeTransfer')}
                  </Button>
                ) : null}
                {(task.status === 'failed' || task.status === 'cancelled') && onRetry ? (
                  <Button
                    variant="secondary"
                    size="sm"
                    icon={<PlayIcon />}
                    onClick={handleRetry}
                    aria-label={t('transfer:retryTransfer')}
                    title={t('transfer:retryTransfer')}
                  >
                    {t('transfer:retryTransfer')}
                  </Button>
                ) : null}
                {task.status === 'completed' && onOpen ? (
                  <Button
                    variant="ghost"
                    size="sm"
                    icon={<CheckIcon />}
                    onClick={handleOpen}
                    aria-label={t('common:action.open')}
                    title={t('common:action.open')}
                  >
                    {t('common:action.open')}
                  </Button>
                ) : null}
                {task.status === 'completed' && onReveal ? (
                  <Button
                    variant="ghost"
                    size="sm"
                    icon={<FolderIcon />}
                    onClick={handleReveal}
                    aria-label={t('transfer:revealInFolder')}
                    title={t('transfer:revealInFolder')}
                  >
                    {t('transfer:revealInFolder')}
                  </Button>
                ) : null}
                {task.status === 'completed' && onDownload ? (
                  <Button
                    variant="ghost"
                    size="sm"
                    icon={<DownloadIcon />}
                    onClick={handleDownload}
                    aria-label={t('transfer:download')}
                    title={t('transfer:download')}
                  >
                    {t('transfer:download')}
                  </Button>
                ) : null}
              </div>
            ) : null}
          </div>
        </div>
      </Card.Body>
    </Card>
  );
}

export const TransferItem = memo(TransferItemInner);
TransferItem.displayName = 'TransferItem';
