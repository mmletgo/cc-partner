/**
 * 提示词三栏 pure view（按 lane 切换布局）。
 *
 * Business Logic（为什么需要）:
 *   公共槽只编辑 shared 正文，不展示预览/原始、不依赖 Agent；
 *   适配槽以 Claude Code 为公共底稿，选中非 Claude 时双列编辑变体；
 *   独有槽保留 当前槽 / 合成预览 / 原始文件 三列。
 *
 * Code Logic（做什么）:
 *   只消费 labels/state/callbacks；禁止 @/api；hooks 不在本视图。
 */

import type { JSX } from 'react';
import { Button, Dialog, StatusMessage } from '@/components/primitives';
import type { AgentTarget } from '@/lib/types/agentHub';
import type { InstructionLane } from '../context/agentHubContext';
import {
  findBlockByMode,
  type InstructionThreePaneState,
} from './instructionThreePane';
import styles from './InstructionThreePaneView.module.css';

/** Claude Code 固定为适配槽公共底稿的权威 agent。 */
const ADAPTED_COMMON_AGENT: AgentTarget = 'claude';

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
  adaptedCommonTitle: string;
  adaptedVariantTitle: string;
  adaptedCommonHint: string;
  adaptedVariantHint: string;
  dualDirtyTitle: string;
  dualDirtyDescription: string;
  useBlocksBaseline: string;
  useOriginalBaseline: string;
  cancel: string;
  blockBodyPlaceholder: string;
  refresh: string;
  commonMarkdown: string;
  saveBlocks: string;
  unsavedDraft: string;
  canonicalDrift: string;
  sourceDrift: string;
  originalReadOnly: string;
  discardAndReload: string;
  reparseConfirmTitle: string;
  reparseConfirmDescription: string;
  reparseConfirm: string;
}

export interface InstructionThreePaneViewProps {
  labels: InstructionThreePaneViewLabels;
  state: InstructionThreePaneState;
  /** 当前 agent：适配变体 / 独有槽与预览跟随此 agent。 */
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
  reparseConfirmOpen: boolean;
  onReparse: () => void;
  onSync: () => void;
  onSaveBlocks: () => void;
  onRetry: () => void;
  onRefresh: () => void;
  onDiscardAndReload: () => void;
  onOriginalChange: (text: string) => void;
  /**
   * 编辑当前 lane 对应槽。
   * 公共写 shared.common；适配写 adapted.common（Claude）或 adapted.variants[agent]；
   * 独有写 targetOnly.variants[agent]。
   */
  onSlotTextChange: (text: string) => void;
  /**
   * 适配槽专用：编辑 Claude 公共底稿（adapted.commonMarkdown）。
   * 仅 instructionLane=adapted 时使用。
   */
  onAdaptedCommonChange?: (text: string) => void;
  /**
   * 适配槽专用：编辑当前 agent 变体（adapted.variants[agent]）。
   * agent=claude 时不展示变体列。
   */
  onAdaptedVariantChange?: (text: string) => void;
  onChooseBaseline: (baseline: 'blocks' | 'original') => void;
  onCancelDualDirty: () => void;
  onConfirmReparse: () => void;
  onCancelReparse: () => void;
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
 * Business Logic: 渲染 toolbar / dual-dirty / 状态消息。
 * Code Logic: 各 lane 共用 chrome，不改变布局分支。
 */
function InstructionChrome(props: {
  labels: InstructionThreePaneViewLabels;
  state: InstructionThreePaneState;
  actionBusy: boolean;
  writeBlocked: boolean;
  writeBlockedReason: string | null;
  actionError: string | null;
  refreshError: string | null;
  dualDirtyOpen: boolean;
  showPath: boolean;
  showSync: boolean;
  onRefresh: () => void;
  onRetry: () => void;
  onDiscardAndReload: () => void;
  onSaveBlocks: () => void;
  onSync: () => void;
  onChooseBaseline: (baseline: 'blocks' | 'original') => void;
  onCancelDualDirty: () => void;
}): JSX.Element {
  const {
    labels,
    state,
    actionBusy,
    writeBlocked,
    writeBlockedReason,
    actionError,
    refreshError,
    dualDirtyOpen,
    showPath,
    showSync,
    onRefresh,
    onRetry,
    onDiscardAndReload,
    onSaveBlocks,
    onSync,
    onChooseBaseline,
    onCancelDualDirty,
  } = props;

  return (
    <>
      <div className={styles.toolbar}>
        <div className={styles.toolbarMeta}>
          {showPath ? (
            <div className={styles.pathRow}>
              <span className={styles.pathLabel}>{labels.pathLabel}</span>
              <code className={styles.pathValue} data-testid="instruction-original-path">
                {state.originalPath ?? labels.noPath}
              </code>
            </div>
          ) : null}
        </div>
        <div className={styles.toolbarActions}>
          <Button
            variant="secondary"
            size="sm"
            loading={actionBusy}
            onClick={onRefresh}
            disabled={actionBusy}
            data-testid="instruction-rescan"
          >
            {labels.refresh}
          </Button>
          <Button
            variant="primary"
            size="sm"
            loading={actionBusy}
            disabled={actionBusy || !state.blocksDirty || state.externalDrift}
            onClick={onSaveBlocks}
            data-testid="instruction-save-blocks"
          >
            {labels.saveBlocks}
          </Button>
          {showSync ? (
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
          ) : null}
        </div>
      </div>

      {writeBlocked && writeBlockedReason ? (
        <StatusMessage tone="warn" data-testid="instruction-write-blocked">
          {writeBlockedReason}
        </StatusMessage>
      ) : null}

      {state.blocksDirty || state.originalDirty ? (
        <StatusMessage tone="info" live="off" data-testid="instruction-unsaved-draft">
          {labels.unsavedDraft}
        </StatusMessage>
      ) : null}

      {state.externalDrift ? (
        <StatusMessage
          tone="warn"
          data-testid="instruction-canonical-drift"
          action={(
            <Button variant="danger" size="sm" onClick={onDiscardAndReload}>
              {labels.discardAndReload}
            </Button>
          )}
        >
          {labels.canonicalDrift}
        </StatusMessage>
      ) : null}

      {state.sourceDrift ? (
        <StatusMessage
          tone="warn"
          live="off"
          data-testid="instruction-source-drift"
          action={
            state.externalDrift ? undefined : (
              <Button variant="danger" size="sm" onClick={onDiscardAndReload}>
                {labels.discardAndReload}
              </Button>
            )
          }
        >
          {labels.sourceDrift}
        </StatusMessage>
      ) : null}

      {refreshError ? (
        <StatusMessage
          tone="warn"
          data-testid="instruction-refresh-error"
          action={(
            <Button variant="secondary" size="sm" onClick={onRetry}>
              {labels.retry}
            </Button>
          )}
        >
          {refreshError}
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
    </>
  );
}

/**
 * Business Logic: 公共槽只展示 shared 编辑区。
 * Code Logic: 单 pane；写入 onSlotTextChange。
 */
function CommonLanePanes(props: {
  labels: InstructionThreePaneViewLabels;
  slotText: string;
  onSlotTextChange: (text: string) => void;
}): JSX.Element {
  const { labels, slotText, onSlotTextChange } = props;
  return (
    <div
      className={`${styles.panes} ${styles.panesSingle}`}
      role="group"
      aria-label={labels.blocksTitle}
      data-testid="instruction-panes-common"
    >
      <section className={styles.pane} data-testid="instruction-pane-blocks">
        <header className={styles.paneHeader}>
          <h2 className={styles.paneTitle}>{labels.blocksTitle}</h2>
        </header>
        <div className={styles.paneBody}>
          <p className={styles.paneHint} data-testid="instruction-slot-hint">
            {labels.slotCommonHint}
          </p>
          <textarea
            className={styles.blockBodyInput}
            value={slotText}
            placeholder={labels.blockBodyPlaceholder}
            aria-label={labels.commonMarkdown}
            data-testid="instruction-slot-textarea"
            onChange={(event) => onSlotTextChange(event.currentTarget.value)}
          />
        </div>
      </section>
    </div>
  );
}

/**
 * Business Logic: 适配槽 — Claude 仅公共底稿；其他 agent 双列（底稿 + 变体）。
 * Code Logic: agent===claude 单列；否则 common 只读展示 + 可编辑 variant。
 */
function AdaptedLanePanes(props: {
  labels: InstructionThreePaneViewLabels;
  agent: AgentTarget;
  commonText: string;
  variantText: string;
  onAdaptedCommonChange: (text: string) => void;
  onAdaptedVariantChange: (text: string) => void;
}): JSX.Element {
  const {
    labels,
    agent,
    commonText,
    variantText,
    onAdaptedCommonChange,
    onAdaptedVariantChange,
  } = props;
  const isClaude = agent === ADAPTED_COMMON_AGENT;

  return (
    <div
      className={`${styles.panes} ${isClaude ? styles.panesSingle : styles.panesDual}`}
      role="group"
      aria-label={labels.blocksTitle}
      data-testid="instruction-panes-adapted"
    >
      <section className={styles.pane} data-testid="instruction-pane-adapted-common">
        <header className={styles.paneHeader}>
          <h2 className={styles.paneTitle}>{labels.adaptedCommonTitle}</h2>
        </header>
        <div className={styles.paneBody}>
          <p className={styles.paneHint} data-testid="instruction-adapted-common-hint">
            {labels.adaptedCommonHint}
          </p>
          <textarea
            className={styles.blockBodyInput}
            value={commonText}
            placeholder={labels.blockBodyPlaceholder}
            aria-label={labels.adaptedCommonTitle}
            data-testid="instruction-adapted-common-textarea"
            // 仅 Claude 可编辑公共底稿；其它 agent 只读查看
            readOnly={!isClaude}
            onChange={(event) => {
              if (!isClaude) return;
              onAdaptedCommonChange(event.currentTarget.value);
            }}
          />
        </div>
      </section>

      {!isClaude ? (
        <section className={styles.pane} data-testid="instruction-pane-adapted-variant">
          <header className={styles.paneHeader}>
            <h2 className={styles.paneTitle}>{labels.adaptedVariantTitle}</h2>
          </header>
          <div className={styles.paneBody}>
            <p className={styles.paneHint} data-testid="instruction-adapted-variant-hint">
              {labels.adaptedVariantHint}
            </p>
            <textarea
              className={styles.blockBodyInput}
              value={variantText}
              placeholder={labels.blockBodyPlaceholder}
              aria-label={labels.adaptedVariantTitle}
              data-testid="instruction-adapted-variant-textarea"
              onChange={(event) => onAdaptedVariantChange(event.currentTarget.value)}
            />
          </div>
        </section>
      ) : null}
    </div>
  );
}

/**
 * Business Logic: 独有槽保持三列 — 当前槽 / 合成预览 / 原始文件。
 * Code Logic: 与历史三栏布局一致。
 */
function ExclusiveLanePanes(props: {
  labels: InstructionThreePaneViewLabels;
  state: InstructionThreePaneState;
  slotText: string;
  onSlotTextChange: (text: string) => void;
  onReparse: () => void;
  reparseDisabled: boolean;
}): JSX.Element {
  const { labels, state, slotText, onSlotTextChange, onReparse, reparseDisabled } = props;

  return (
    <div
      className={styles.panes}
      role="group"
      aria-label={labels.blocksTitle}
      data-testid="instruction-panes-exclusive"
    >
      <section className={styles.pane} data-testid="instruction-pane-blocks">
        <header className={styles.paneHeader}>
          <h2 className={styles.paneTitle}>{labels.blocksTitle}</h2>
        </header>
        <div className={styles.paneBody}>
          <p className={styles.paneHint} data-testid="instruction-slot-hint">
            {labels.slotExclusiveHint}
          </p>
          <textarea
            className={styles.blockBodyInput}
            value={slotText}
            placeholder={labels.blockBodyPlaceholder}
            aria-label={labels.commonMarkdown}
            data-testid="instruction-slot-textarea"
            onChange={(event) => onSlotTextChange(event.currentTarget.value)}
          />
        </div>
      </section>

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

      <section className={styles.pane} data-testid="instruction-pane-original">
        <header className={styles.paneHeader}>
          <h2 className={styles.paneTitle}>{labels.originalTitle}</h2>
          <Button
            variant="secondary"
            size="sm"
            onClick={onReparse}
            disabled={reparseDisabled}
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
            readOnly
          />
          <p className={styles.paneHint}>{labels.originalReadOnly}</p>
        </div>
      </section>
    </div>
  );
}

/**
 * Business Logic: 按 instructionLane 渲染对应布局；loading/error 守卫在视图内。
 * Code Logic: 纯 props 渲染。
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
    reparseConfirmOpen,
    onReparse,
    onSync,
    onSaveBlocks,
    onRetry,
    onRefresh,
    onDiscardAndReload,
    onSlotTextChange,
    onAdaptedCommonChange,
    onAdaptedVariantChange,
    onChooseBaseline,
    onCancelDualDirty,
    onConfirmReparse,
    onCancelReparse,
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

  const sharedBlock = findBlockByMode(state.blocks, 'shared');
  const adaptedBlock = findBlockByMode(state.blocks, 'adapted');
  const exclusiveBlock = findBlockByMode(state.blocks, 'targetOnly');

  const commonSlotText = sharedBlock?.commonMarkdown ?? '';
  // 适配公共底稿：优先 adapted.commonMarkdown；空时不自动回落 shared，保持槽隔离
  const adaptedCommonText = adaptedBlock?.commonMarkdown ?? '';
  // 变体仅取 variants[agent]，空字符串表示「无变体，投影时回落 common」
  const adaptedVariantText =
    agent === ADAPTED_COMMON_AGENT
      ? ''
      : (adaptedBlock?.variants[agent] ?? '');
  const exclusiveSlotText = exclusiveBlock?.variants[agent] ?? '';

  // 路径仅在独有槽（展示原始文件）有意义；同步写盘各 lane 均可用
  const showPath = instructionLane === 'exclusive';
  const showSync = false;

  return (
    <div className={styles.root} data-testid="instruction-three-pane">
      <InstructionChrome
        labels={labels}
        state={state}
        actionBusy={actionBusy}
        writeBlocked={writeBlocked}
        writeBlockedReason={writeBlockedReason}
        actionError={actionError}
        refreshError={error}
        dualDirtyOpen={dualDirtyOpen}
        showPath={showPath}
        showSync={showSync}
        onRefresh={onRefresh}
        onRetry={onRetry}
        onDiscardAndReload={onDiscardAndReload}
        onSaveBlocks={onSaveBlocks}
        onSync={onSync}
        onChooseBaseline={onChooseBaseline}
        onCancelDualDirty={onCancelDualDirty}
      />

      {/* 顶部 lane 提示：适配/公共保留简述 */}
      {instructionLane !== 'exclusive' ? (
        <p className={styles.laneBanner} data-testid="instruction-slot-hint">
          {slotHint(labels, instructionLane)}
        </p>
      ) : null}

      {instructionLane === 'common' ? (
        <CommonLanePanes
          labels={labels}
          slotText={commonSlotText}
          onSlotTextChange={onSlotTextChange}
        />
      ) : null}

      {instructionLane === 'adapted' ? (
        <AdaptedLanePanes
          labels={labels}
          agent={agent}
          commonText={adaptedCommonText}
          variantText={adaptedVariantText}
          onAdaptedCommonChange={onAdaptedCommonChange ?? onSlotTextChange}
          onAdaptedVariantChange={onAdaptedVariantChange ?? onSlotTextChange}
        />
      ) : null}

      {instructionLane === 'exclusive' ? (
        <ExclusiveLanePanes
          labels={labels}
          state={state}
          slotText={exclusiveSlotText}
          onSlotTextChange={onSlotTextChange}
          onReparse={onReparse}
          reparseDisabled={actionBusy || state.sourceDrift || state.externalDrift}
        />
      ) : null}

      <Dialog
        open={reparseConfirmOpen}
        titleId="instruction-reparse-confirm-title"
        onClose={onCancelReparse}
      >
        <div className={styles.dualDirty} data-testid="instruction-reparse-confirm">
          <h2 id="instruction-reparse-confirm-title" className={styles.dualDirtyTitle}>
            {labels.reparseConfirmTitle}
          </h2>
          <p className={styles.dualDirtyDesc}>{labels.reparseConfirmDescription}</p>
          <div className={styles.dualDirtyActions}>
            <Button variant="secondary" size="sm" onClick={onCancelReparse}>
              {labels.cancel}
            </Button>
            <Button variant="danger" size="sm" onClick={onConfirmReparse}>
              {labels.reparseConfirm}
            </Button>
          </div>
        </div>
      </Dialog>
    </div>
  );
}
