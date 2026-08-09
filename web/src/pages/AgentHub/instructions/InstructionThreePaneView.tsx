/**
 * 提示词三栏 pure view。
 *
 * Business Logic（为什么需要）:
 *   桌面默认同时展示 ① 当前三槽 ② 合成预览 ③ 原始文件；
 *   槽由壳层 instructionLane 选择；「从原始导入为公共」仅在原始栏。
 *
 * Code Logic（做什么）:
 *   只消费 labels/state/callbacks；禁止 @/api；hooks 不在本视图。
 */

import type { JSX } from 'react';
import { Button, StatusMessage } from '@/components/primitives';
import type { AgentTarget } from '@/lib/types/agentHub';
import type { InstructionLane } from '../context/agentHubContext';
import {
  findBlockByMode,
  type InstructionThreePaneState,
} from './instructionThreePane';
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
  slotCommonHint: string;
  slotAdaptedHint: string;
  slotExclusiveHint: string;
  dualDirtyTitle: string;
  dualDirtyDescription: string;
  useBlocksBaseline: string;
  useOriginalBaseline: string;
  cancel: string;
  blockBodyPlaceholder: string;
  refresh: string;
  commonMarkdown: string;
  saveBlocks: string;
}

export interface InstructionThreePaneViewProps {
  labels: InstructionThreePaneViewLabels;
  state: InstructionThreePaneState;
  /** 当前 agent：适配/独有槽与预览跟随此 agent。 */
  agent: AgentTarget;
  /** 壳层选择的三槽 lane。 */
  instructionLane: InstructionLane;
  loading: boolean;
  error: string | null;
  actionError: string | null;
  actionBusy: boolean;
  writeBlocked: boolean;
  writeBlockedReason: string | null;
  dualDirtyOpen: boolean;
  onReparse: () => void;
  onSync: () => void;
  onSaveBlocks: () => void;
  onRetry: () => void;
  onRefresh: () => void;
  onOriginalChange: (text: string) => void;
  /** 编辑当前 lane 对应槽的正文。 */
  onSlotTextChange: (text: string) => void;
  onChooseBaseline: (baseline: 'blocks' | 'original') => void;
  onCancelDualDirty: () => void;
}

function laneToMode(
  lane: InstructionLane,
): 'shared' | 'adapted' | 'targetOnly' {
  switch (lane) {
    case 'common':
      return 'shared';
    case 'adapted':
      return 'adapted';
    case 'exclusive':
      return 'targetOnly';
  }
}

function slotHint(
  labels: InstructionThreePaneViewLabels,
  lane: InstructionLane,
): string {
  switch (lane) {
    case 'common':
      return labels.slotCommonHint;
    case 'adapted':
      return labels.slotAdaptedHint;
    case 'exclusive':
      return labels.slotExclusiveHint;
  }
}

/**
 * Business Logic: 渲染三栏编辑器；loading/error 守卫在视图内展示，不阻断父层 hooks。
 * Code Logic: 纯 props 渲染；左栏单槽编辑，reparse 仅绑在原始栏。
 */
export function InstructionThreePaneView(props: InstructionThreePaneViewProps): JSX.Element {
  const {
    labels,
    state,
    agent,
    instructionLane,
    loading,
    error,
    actionError,
    actionBusy,
    writeBlocked,
    writeBlockedReason,
    dualDirtyOpen,
    onReparse,
    onSync,
    onSaveBlocks,
    onRetry,
    onRefresh,
    onOriginalChange,
    onSlotTextChange,
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

  const mode = laneToMode(instructionLane);
  const block = findBlockByMode(state.blocks, mode);
  let slotText = '';
  if (mode === 'shared') {
    slotText = block?.commonMarkdown ?? '';
  } else if (mode === 'adapted') {
    // 无 variant 时展示 common 作底稿
    slotText = block?.variants[agent] ?? block?.commonMarkdown ?? '';
  } else {
    slotText = block?.variants[agent] ?? '';
  }
  const slotEmpty = slotText.trim().length === 0 && !block;

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
            onClick={onSaveBlocks}
            data-testid="instruction-save-blocks"
          >
            {labels.saveBlocks}
          </Button>
          <Button
            variant="secondary"
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
        {/* ① 当前三槽 */}
        <section className={styles.pane} data-testid="instruction-pane-blocks">
          <header className={styles.paneHeader}>
            <h2 className={styles.paneTitle}>{labels.blocksTitle}</h2>
          </header>
          <div className={styles.paneBody}>
            <p className={styles.paneHint} data-testid="instruction-slot-hint">
              {slotHint(labels, instructionLane)}
            </p>
            {slotEmpty ? (
              <p className={styles.empty} data-testid="instruction-blocks-empty">
                {labels.emptyBlocks}
              </p>
            ) : (
              <div className={styles.blockList} data-testid="instruction-block-list">
                <article
                  className={styles.blockCard}
                  data-testid={`instruction-block-slot-${instructionLane}`}
                >
                  <textarea
                    className={styles.blockBodyInput}
                    value={slotText}
                    placeholder={labels.blockBodyPlaceholder}
                    aria-label={labels.commonMarkdown}
                    data-testid="instruction-slot-textarea"
                    onChange={(event) => onSlotTextChange(event.currentTarget.value)}
                  />
                </article>
              </div>
            )}
            {/* 空槽也保留可编辑入口，避免只能靠 reparse 才能输入 */}
            {slotEmpty ? (
              <textarea
                className={styles.blockBodyInput}
                value={slotText}
                placeholder={labels.blockBodyPlaceholder}
                aria-label={labels.commonMarkdown}
                data-testid="instruction-slot-textarea"
                onChange={(event) => onSlotTextChange(event.currentTarget.value)}
              />
            ) : null}
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
