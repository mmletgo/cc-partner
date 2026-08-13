/**
 * 提示词 AI 辅助改写方向输入 Dialog。
 *
 * Business Logic（为什么需要）:
 *   用户需要先写清改写方向，再调用本机 Claude；进行中不得误关导致重复提交。
 *
 * Code Logic（做什么）:
 *   纯 props Dialog；空方向禁用确认；busy 时锁 Escape/遮罩关闭。
 */

import { useRef, type JSX } from 'react';
import { Button, Dialog, StatusMessage } from '@/components/primitives';
import styles from './AiReviseInstructionDialog.module.css';

export interface AiReviseInstructionDialogProps {
  open: boolean;
  title: string;
  description: string;
  directionLabel: string;
  directionPlaceholder: string;
  confirmLabel: string;
  cancelLabel: string;
  direction: string;
  error: string | null;
  busy: boolean;
  onDirectionChange: (value: string) => void;
  onCancel: () => void;
  onConfirm: () => void;
}

/**
 * Business Logic: 收集改写方向并确认调用 Claude。
 * Code Logic: 默认 Dialog padding；textarea 自管；hooks 在 early return 前。
 */
export function AiReviseInstructionDialog(
  props: AiReviseInstructionDialogProps,
): JSX.Element {
  const {
    open,
    title,
    description,
    directionLabel,
    directionPlaceholder,
    confirmLabel,
    cancelLabel,
    direction,
    error,
    busy,
    onDirectionChange,
    onCancel,
    onConfirm,
  } = props;
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const canConfirm = direction.trim().length > 0 && !busy;

  return (
    <Dialog
      open={open}
      titleId="instruction-ai-revise-title"
      onClose={onCancel}
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
      initialFocusRef={textareaRef}
    >
      <div className={styles.body} data-testid="instruction-ai-revise-dialog">
        <h2 id="instruction-ai-revise-title" className={styles.title}>
          {title}
        </h2>
        <p className={styles.description}>{description}</p>
        <label className={styles.field} htmlFor="instruction-ai-revise-direction">
          <span className={styles.fieldLabel}>{directionLabel}</span>
          <textarea
            id="instruction-ai-revise-direction"
            ref={textareaRef}
            className={styles.direction}
            value={direction}
            placeholder={directionPlaceholder}
            disabled={busy}
            data-testid="instruction-ai-revise-direction"
            onChange={(event) => onDirectionChange(event.currentTarget.value)}
          />
        </label>
        {error ? (
          <StatusMessage tone="danger" data-testid="instruction-ai-revise-error">
            {error}
          </StatusMessage>
        ) : null}
        <div className={styles.actions}>
          <Button
            variant="ghost"
            size="sm"
            disabled={busy}
            onClick={onCancel}
            data-testid="instruction-ai-revise-cancel"
          >
            {cancelLabel}
          </Button>
          <Button
            variant="primary"
            size="sm"
            loading={busy}
            disabled={!canConfirm}
            onClick={onConfirm}
            data-testid="instruction-ai-revise-confirm"
          >
            {confirmLabel}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
