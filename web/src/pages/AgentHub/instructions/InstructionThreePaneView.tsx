/**
 * 提示词三栏 pure view（按 lane 切换布局）。
 *
 * Business Logic（为什么需要）:
 *   公共槽只编辑 shared 正文，不展示预览/原始、不依赖 Agent；
 *   适配槽按当前 Agent 编辑自身适配变体，并提供「适配到其他 Agent」；
 *   独有槽保留 当前槽 / 合成预览 / 原始文件 三列，原始栏提供「分析拆解」。
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
  resolveAdaptedSlotText,
  type InstructionThreePaneState,
} from './instructionThreePane';
import styles from './InstructionThreePaneView.module.css';

/** 三栏视图文案。 */
export interface InstructionThreePaneViewLabels {
  blocksTitle: string;
  previewTitle: string;
  originalTitle: string;
  /** 独有页「分析拆解」按钮。 */
  analyzeDecompose: string;
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
  commonMarkdown: string;
  saveBlocks: string;
  /** 适配页：把当前 agent 适配内容改写到其他 agent。 */
  adaptToOtherAgents: string;
  unsavedDraft: string;
  canonicalDrift: string;
  sourceDrift: string;
  originalReadOnly: string;
  discardAndReload: string;
  analyzeConfirmTitle: string;
  analyzeConfirmDescription: string;
  analyzeConfirm: string;
}

export interface InstructionThreePaneViewProps {
  labels: InstructionThreePaneViewLabels;
  state: InstructionThreePaneState;
  /** 当前 agent：适配 / 独有槽与预览跟随此 agent。 */
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
  analyzeConfirmOpen: boolean;
  onAnalyzeDecompose: () => void;
  onAdaptToOtherAgents: () => void;
  onSaveBlocks: () => void;
  onRetry: () => void;
  onDiscardAndReload: () => void;
  /**
   * 编辑当前 lane 对应槽。
   * 公共写 shared.common；适配写 adapted.variants[agent]；独有写 targetOnly.variants[agent]。
   */
  onSlotTextChange: (text: string) => void;
  onChooseBaseline: (baseline: 'blocks' | 'original') => void;
  onCancelDualDirty: () => void;
  onConfirmAnalyze: () => void;
  onCancelAnalyze: () => void;
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
 * Code Logic: 各 lane 共用 chrome；公共/适配不展示路径、重新扫描与写入原始。
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
  showAdaptToOthers: boolean;
  onRetry: () => void;
  onDiscardAndReload: () => void;
  onSaveBlocks: () => void;
  onAdaptToOtherAgents: () => void;
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
    showAdaptToOthers,
    onRetry,
    onDiscardAndReload,
    onSaveBlocks,
    onAdaptToOtherAgents,
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
            variant="primary"
            size="sm"
            loading={actionBusy}
            disabled={actionBusy || !state.blocksDirty || state.externalDrift}
            onClick={onSaveBlocks}
            data-testid="instruction-save-blocks"
          >
            {labels.saveBlocks}
          </Button>
          {showAdaptToOthers ? (
            <Button
              variant="secondary"
              size="sm"
              loading={actionBusy}
              disabled={actionBusy || writeBlocked}
              onClick={onAdaptToOtherAgents}
              data-testid="instruction-adapt-to-other-agents"
            >
              {labels.adaptToOtherAgents}
            </Button>
          ) : null}
        </div>
      </div>

      {showAdaptToOthers && writeBlocked && writeBlockedReason ? (
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
 * Business Logic: 适配槽 — 当前 agent 单列编辑自身适配正文。
 * Code Logic: 不再以 Claude 为公共底稿双列。
 */
function AdaptedLanePanes(props: {
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
      data-testid="instruction-panes-adapted"
    >
      <section className={styles.pane} data-testid="instruction-pane-adapted">
        <header className={styles.paneHeader}>
          <h2 className={styles.paneTitle}>{labels.blocksTitle}</h2>
        </header>
        <div className={styles.paneBody}>
          <p className={styles.paneHint} data-testid="instruction-slot-hint">
            {labels.slotAdaptedHint}
          </p>
          <textarea
            className={styles.blockBodyInput}
            value={slotText}
            placeholder={labels.blockBodyPlaceholder}
            aria-label={labels.blocksTitle}
            data-testid="instruction-adapted-textarea"
            onChange={(event) => onSlotTextChange(event.currentTarget.value)}
          />
        </div>
      </section>
    </div>
  );
}

/**
 * Business Logic: 独有槽保持三列 — 当前槽 / 合成预览 / 原始文件。
 * Code Logic: 原始栏提供「分析拆解」，不再提供「从原始导入为公共」。
 */
function ExclusiveLanePanes(props: {
  labels: InstructionThreePaneViewLabels;
  state: InstructionThreePaneState;
  slotText: string;
  onSlotTextChange: (text: string) => void;
  onAnalyzeDecompose: () => void;
  analyzeDisabled: boolean;
}): JSX.Element {
  const {
    labels,
    state,
    slotText,
    onSlotTextChange,
    onAnalyzeDecompose,
    analyzeDisabled,
  } = props;

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
            onClick={onAnalyzeDecompose}
            disabled={analyzeDisabled}
            data-testid="instruction-analyze-decompose"
          >
            {labels.analyzeDecompose}
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
    analyzeConfirmOpen,
    onAnalyzeDecompose,
    onAdaptToOtherAgents,
    onSaveBlocks,
    onRetry,
    onDiscardAndReload,
    onSlotTextChange,
    onChooseBaseline,
    onCancelDualDirty,
    onConfirmAnalyze,
    onCancelAnalyze,
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
  const adaptedSlotText = resolveAdaptedSlotText(adaptedBlock, agent);
  const exclusiveSlotText = exclusiveBlock?.variants[agent] ?? '';

  const showPath = instructionLane === 'exclusive';
  const showAdaptToOthers = instructionLane === 'adapted';

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
        showAdaptToOthers={showAdaptToOthers}
        onRetry={onRetry}
        onDiscardAndReload={onDiscardAndReload}
        onSaveBlocks={onSaveBlocks}
        onAdaptToOtherAgents={onAdaptToOtherAgents}
        onChooseBaseline={onChooseBaseline}
        onCancelDualDirty={onCancelDualDirty}
      />

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
          slotText={adaptedSlotText}
          onSlotTextChange={onSlotTextChange}
        />
      ) : null}

      {instructionLane === 'exclusive' ? (
        <ExclusiveLanePanes
          labels={labels}
          state={state}
          slotText={exclusiveSlotText}
          onSlotTextChange={onSlotTextChange}
          onAnalyzeDecompose={onAnalyzeDecompose}
          analyzeDisabled={
            actionBusy ||
            state.sourceDrift ||
            state.externalDrift ||
            state.originalText.trim().length === 0
          }
        />
      ) : null}

      <Dialog
        open={analyzeConfirmOpen}
        titleId="instruction-analyze-confirm-title"
        onClose={onCancelAnalyze}
      >
        <div className={styles.dualDirty} data-testid="instruction-analyze-confirm">
          <h2 id="instruction-analyze-confirm-title" className={styles.dualDirtyTitle}>
            {labels.analyzeConfirmTitle}
          </h2>
          <p className={styles.dualDirtyDesc}>{labels.analyzeConfirmDescription}</p>
          <div className={styles.dualDirtyActions}>
            <Button variant="secondary" size="sm" onClick={onCancelAnalyze}>
              {labels.cancel}
            </Button>
            <Button variant="primary" size="sm" onClick={onConfirmAnalyze}>
              {labels.analyzeConfirm}
            </Button>
          </div>
        </div>
      </Dialog>
    </div>
  );
}
