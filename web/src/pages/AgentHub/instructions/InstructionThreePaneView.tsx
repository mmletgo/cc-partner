/**
 * 提示词三栏 pure view。
 *
 * Business Logic（为什么需要）:
 *   桌面默认同时展示 ① 块 ② 合成预览 ③ 原始文件；
 *   「从原始重新解析块」仅在原始栏，同步为写盘主入口。
 *
 * Code Logic（做什么）:
 *   只消费 labels/state/callbacks；禁止 @/api；hooks 不在本视图。
 */

import type { JSX } from 'react';
import { Button, StatusMessage } from '@/components/primitives';
import type { InstructionBlockDraft, InstructionThreePaneState } from './instructionThreePane';
import styles from './InstructionThreePaneView.module.css';

/** 三栏视图文案。 */
export interface InstructionThreePaneViewLabels {
  blocksTitle: string;
  previewTitle: string;
  originalTitle: string;
  reparseFromOriginal: string;
  syncToNative: string;
  emptyBlocks: string;
  emptyPreview: string;
  emptyOriginal: string;
  pathLabel: string;
  noPath: string;
  loading: string;
  retry: string;
  previewReadOnly: string;
  addBlock: string;
  dualDirtyTitle: string;
  dualDirtyDescription: string;
  useBlocksBaseline: string;
  useOriginalBaseline: string;
  cancel: string;
  blockTitlePlaceholder: string;
  blockBodyPlaceholder: string;
  refresh: string;
}

export interface InstructionThreePaneViewProps {
  labels: InstructionThreePaneViewLabels;
  state: InstructionThreePaneState;
  loading: boolean;
  error: string | null;
  actionError: string | null;
  actionBusy: boolean;
  writeBlocked: boolean;
  writeBlockedReason: string | null;
  dualDirtyOpen: boolean;
  onReparse: () => void;
  onSync: () => void;
  onRetry: () => void;
  onRefresh: () => void;
  onOriginalChange: (text: string) => void;
  onBlockChange: (id: string, patch: Partial<Omit<InstructionBlockDraft, 'id'>>) => void;
  onAddBlock: () => void;
  onChooseBaseline: (baseline: 'blocks' | 'original') => void;
  onCancelDualDirty: () => void;
}

/**
 * Business Logic: 渲染三栏编辑器；loading/error 守卫在视图内展示，不阻断父层 hooks。
 * Code Logic: 纯 props 渲染；reparse 仅绑在原始栏。
 */
export function InstructionThreePaneView(props: InstructionThreePaneViewProps): JSX.Element {
  const {
    labels,
    state,
    loading,
    error,
    actionError,
    actionBusy,
    writeBlocked,
    writeBlockedReason,
    dualDirtyOpen,
    onReparse,
    onSync,
    onRetry,
    onRefresh,
    onOriginalChange,
    onBlockChange,
    onAddBlock,
    onChooseBaseline,
    onCancelDualDirty,
  } = props;

  if (loading && !state.originalText && state.blocks.length === 0 && !error) {
    return (
      <StatusMessage tone="info" data-testid="instruction-three-pane-loading">
        {labels.loading}
      </StatusMessage>
    );
  }

  if (error && !state.originalPath && state.blocks.length === 0 && !state.originalText) {
    return (
      <StatusMessage
        tone="danger"
        data-testid="instruction-three-pane-error"
        action={
          <Button size="sm" onClick={onRetry}>
            {labels.retry}
          </Button>
        }
      >
        {error}
      </StatusMessage>
    );
  }

  return (
    <div className={styles.root} data-testid="instruction-three-pane">
      <div className={styles.toolbar}>
        <div className={styles.toolbarMeta}>
          <div className={styles.pathRow}>
            <span className={styles.pathLabel}>{labels.pathLabel}</span>
            <code className={styles.pathValue} data-testid="instruction-original-path">
              {state.originalPath ?? labels.noPath}
            </code>
          </div>
        </div>
        <div className={styles.toolbarActions}>
          <Button
            variant="secondary"
            size="sm"
            loading={actionBusy}
            onClick={onRefresh}
            data-testid="instruction-rescan"
          >
            {labels.refresh}
          </Button>
          <Button
            variant="primary"
            size="sm"
            loading={actionBusy}
            disabled={writeBlocked}
            onClick={onSync}
            data-testid="instruction-sync-to-native"
          >
            {labels.syncToNative}
          </Button>
        </div>
      </div>

      {writeBlocked && writeBlockedReason ? (
        <StatusMessage tone="warn" data-testid="instruction-write-blocked">
          {writeBlockedReason}
        </StatusMessage>
      ) : null}

      {actionError ? (
        <StatusMessage tone="danger" data-testid="instruction-action-error">
          {actionError}
        </StatusMessage>
      ) : null}

      {dualDirtyOpen ? (
        <div className={styles.dualDirty} data-testid="instruction-dual-dirty">
          <h3 className={styles.dualDirtyTitle}>{labels.dualDirtyTitle}</h3>
          <p className={styles.dualDirtyDesc}>{labels.dualDirtyDescription}</p>
          <div className={styles.dualDirtyActions}>
            <Button
              variant="secondary"
              size="sm"
              onClick={() => onChooseBaseline('blocks')}
              data-testid="instruction-baseline-blocks"
            >
              {labels.useBlocksBaseline}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              onClick={() => onChooseBaseline('original')}
              data-testid="instruction-baseline-original"
            >
              {labels.useOriginalBaseline}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={onCancelDualDirty}
              data-testid="instruction-baseline-cancel"
            >
              {labels.cancel}
            </Button>
          </div>
        </div>
      ) : null}

      <div className={styles.panes} role="group" aria-label={labels.blocksTitle}>
        {/* ① 块 */}
        <section className={styles.pane} data-testid="instruction-pane-blocks">
          <header className={styles.paneHeader}>
            <h2 className={styles.paneTitle}>{labels.blocksTitle}</h2>
            <Button
              variant="ghost"
              size="sm"
              onClick={onAddBlock}
              data-testid="instruction-add-block"
            >
              {labels.addBlock}
            </Button>
          </header>
          <div className={styles.paneBody}>
            {state.blocks.length === 0 ? (
              <p className={styles.empty} data-testid="instruction-blocks-empty">
                {labels.emptyBlocks}
              </p>
            ) : (
              <div className={styles.blockList} data-testid="instruction-block-list">
                {state.blocks.map((block) => (
                  <article
                    key={block.id}
                    className={styles.blockCard}
                    data-testid={`instruction-block-${block.id}`}
                  >
                    <input
                      className={styles.blockTitleInput}
                      value={block.title}
                      placeholder={labels.blockTitlePlaceholder}
                      aria-label={labels.blockTitlePlaceholder}
                      onChange={(event) =>
                        onBlockChange(block.id, { title: event.currentTarget.value })
                      }
                    />
                    <textarea
                      className={styles.blockBodyInput}
                      value={block.body}
                      placeholder={labels.blockBodyPlaceholder}
                      aria-label={labels.blockBodyPlaceholder}
                      onChange={(event) =>
                        onBlockChange(block.id, { body: event.currentTarget.value })
                      }
                    />
                  </article>
                ))}
              </div>
            )}
          </div>
        </section>

        {/* ② 合成预览（只读） */}
        <section className={styles.pane} data-testid="instruction-pane-preview">
          <header className={styles.paneHeader}>
            <h2 className={styles.paneTitle}>{labels.previewTitle}</h2>
            <p className={styles.paneHint}>{labels.previewReadOnly}</p>
          </header>
          <div className={styles.paneBody}>
            {state.previewText.trim().length === 0 ? (
              <p className={styles.empty} data-testid="instruction-preview-empty">
                {labels.emptyPreview}
              </p>
            ) : (
              <pre className={styles.previewBody} data-testid="instruction-preview-body">
                {state.previewText}
              </pre>
            )}
          </div>
        </section>

        {/* ③ 原始文件 — reparse 仅在此栏 */}
        <section className={styles.pane} data-testid="instruction-pane-original">
          <header className={styles.paneHeader}>
            <h2 className={styles.paneTitle}>{labels.originalTitle}</h2>
            <Button
              variant="secondary"
              size="sm"
              onClick={onReparse}
              data-testid="instruction-reparse-from-original"
            >
              {labels.reparseFromOriginal}
            </Button>
          </header>
          <div className={styles.paneBody}>
            {state.originalText.length === 0 && !state.originalDirty ? (
              <p className={styles.empty} data-testid="instruction-original-empty">
                {labels.emptyOriginal}
              </p>
            ) : null}
            <textarea
              className={styles.originalInput}
              value={state.originalText}
              aria-label={labels.originalTitle}
              data-testid="instruction-original-textarea"
              onChange={(event) => onOriginalChange(event.currentTarget.value)}
            />
          </div>
        </section>
      </div>
    </div>
  );
}
