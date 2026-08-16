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

import { useEffect, useRef, type JSX, type RefObject } from 'react';
import { Button, Dialog, StatusMessage } from '@/components/primitives';
import { SparkleIcon, HistoryIcon } from '@/lib/icons';
import type { AgentTarget } from '@/lib/types/agentHub';
import type { InstructionLane } from '../context/agentHubContext';
import { AiReviseInstructionDialog } from './AiReviseInstructionDialog';
import { VersionHistoryDrawer } from '@/components/domain/VersionHistoryDrawer';
import {
  findBlockByMode,
  resolveAdaptedSlotText,
  type InstructionAiReviseFeedback,
  type InstructionBusyAction,
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
  /** 三页共用：AI 按方向改当前槽。 */
  aiRevise: string;
  aiReviseTitle: string;
  aiReviseDescriptionCommon: string;
  aiReviseDescriptionExclusive: string;
  aiReviseDescriptionAdapted: string;
  aiReviseDirectionLabel: string;
  aiReviseDirectionPlaceholder: string;
  aiReviseConfirm: string;
  aiReviseSavedAndLocated: string;
  aiReviseSavedOtherAgents: string;
  aiReviseSavedNoChange: string;
  /** 适配页：把当前 agent 适配内容改写到其他 agent。 */
  adaptToOtherAgents: string;
  /**
   * 独有页：把合成预览（或 dual-dirty 选定基线）写入当前 Agent 原始文件。
   * Business Logic: 保存三槽只更新 Canonical；本按钮走 preview→apply 真正写盘。
   */
  syncToNative: string;
  unsavedDraft: string;
  canonicalDrift: string;
  sourceDrift: string;
  originalReadOnly: string;
  discardAndReload: string;
  analyzeConfirmTitle: string;
  analyzeConfirmDescription: string;
  analyzeConfirm: string;
  /** 按 lane 打开当前槽历史抽屉的按钮文案。 */
  slotHistoryCommon: string;
  slotHistoryAdapted: string;
  slotHistoryTargetOnly: string;
  /** 抽屉内的复制成功反馈（与 VersionHistoryDrawer 共享命名空间对齐）。 */
  slotHistoryCopied: string;
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
  /** 任一动作互斥禁用（含 refresh）。 */
  actionBusy: boolean;
  /**
   * 当前具体动作；spinner 只挂在对应按钮上。
   * Code Logic: null 时任何按钮都不转圈（即使 actionBusy 因 refresh 为 true）。
   */
  busyAction: InstructionBusyAction | null;
  writeBlocked: boolean;
  writeBlockedReason: string | null;
  dualDirtyOpen: boolean;
  analyzeConfirmOpen: boolean;
  aiReviseOpen: boolean;
  aiReviseDirection: string;
  aiReviseError: string | null;
  aiReviseFeedback: InstructionAiReviseFeedback | null;
  /** Canonical 漂移/截断/非本机时禁用，不看原生写入门禁。 */
  aiReviseDisabled: boolean;
  onAnalyzeDecompose: () => void;
  onAdaptToOtherAgents: () => void;
  onSaveBlocks: () => void;
  onOpenAiRevise: () => void;
  onAiReviseDirectionChange: (value: string) => void;
  onCancelAiRevise: () => void;
  onConfirmAiRevise: () => void;
  /**
   * 独有页：合成预览 → 写入当前 Agent 原始文件（内部 save + preview + apply）。
   * 双脏分歧时由 controller 打开基线选择，不直接写盘。
   */
  onRequestSync: () => void;
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
  /** 三槽历史抽屉状态机。 */
  slotHistoryOpen: boolean;
  slotHistoryLoading: boolean;
  slotHistoryError: string | null;
  slotHistoryActionError: string | null;
  restoringSlotVersionId: string | null;
  slotHistoryVersions: import('@/lib/types/core').ContentVersion[];
  onOpenSlotHistory: () => void;
  onCloseSlotHistory: () => void;
  onCopySlotVersion: (version: import('@/lib/types/core').ContentVersion) => void;
  onRestoreSlotVersion: (version: import('@/lib/types/core').ContentVersion) => void;
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
 * Code Logic: 各 lane 共用 chrome；公共/适配不展示路径与重新扫描；
 *   写入原始仅在独有 lane（showWriteToNative）露出。
 */
function InstructionChrome(props: {
  labels: InstructionThreePaneViewLabels;
  state: InstructionThreePaneState;
  actionBusy: boolean;
  busyAction: InstructionBusyAction | null;
  writeBlocked: boolean;
  writeBlockedReason: string | null;
  actionError: string | null;
  aiReviseFeedback: InstructionAiReviseFeedback | null;
  refreshError: string | null;
  dualDirtyOpen: boolean;
  showPath: boolean;
  showAdaptToOthers: boolean;
  showWriteToNative: boolean;
  aiReviseDisabled: boolean;
  instructionLane: InstructionLane;
  onRetry: () => void;
  onDiscardAndReload: () => void;
  onSaveBlocks: () => void;
  onOpenAiRevise: () => void;
  onAdaptToOtherAgents: () => void;
  onRequestSync: () => void;
  onChooseBaseline: (baseline: 'blocks' | 'original') => void;
  onCancelDualDirty: () => void;
  onOpenSlotHistory: () => void;
}): JSX.Element {
  const {
    labels,
    state,
    actionBusy,
    busyAction,
    writeBlocked,
    writeBlockedReason,
    actionError,
    aiReviseFeedback,
    refreshError,
    dualDirtyOpen,
    showPath,
    showAdaptToOthers,
    showWriteToNative,
    aiReviseDisabled,
    instructionLane,
    onRetry,
    onDiscardAndReload,
    onSaveBlocks,
    onOpenAiRevise,
    onAdaptToOtherAgents,
    onRequestSync,
    onChooseBaseline,
    onCancelDualDirty,
    onOpenSlotHistory,
  } = props;

  /**
   * Business Logic: 按当前 lane 决定显示哪个槽的历史按钮（per-slot 隔离）。
   * Code Logic: shared/adapted/exclusive 三档互斥，none lane 不显示。
   */
  const slotHistoryLabel: string | null = (() => {
    switch (instructionLane) {
      case 'common':
        return labels.slotHistoryCommon;
      case 'adapted':
        return labels.slotHistoryAdapted;
      case 'exclusive':
        return labels.slotHistoryTargetOnly;
      default:
        return null;
    }
  })();
  const slotHistoryTestId = (() => {
    switch (instructionLane) {
      case 'common':
        return 'instruction-slot-history-shared';
      case 'adapted':
        return 'instruction-slot-history-adapted';
      case 'exclusive':
        return 'instruction-slot-history-targetOnly';
      default:
        return null;
    }
  })();

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
            loading={busyAction === 'save'}
            disabled={actionBusy || !state.blocksDirty || state.externalDrift}
            onClick={onSaveBlocks}
            data-testid="instruction-save-blocks"
          >
            {labels.saveBlocks}
          </Button>
          <Button
            variant="secondary"
            size="sm"
            icon={<SparkleIcon />}
            loading={busyAction === 'revise'}
            disabled={actionBusy || aiReviseDisabled}
            onClick={onOpenAiRevise}
            data-testid="instruction-ai-revise"
          >
            {labels.aiRevise}
          </Button>
          {slotHistoryLabel && slotHistoryTestId ? (
            <Button
              variant="ghost"
              size="sm"
              icon={<HistoryIcon />}
              disabled={actionBusy}
              onClick={onOpenSlotHistory}
              data-testid={slotHistoryTestId}
            >
              {slotHistoryLabel}
            </Button>
          ) : null}
          {showWriteToNative ? (
            <Button
              variant="secondary"
              size="sm"
              loading={busyAction === 'sync'}
              disabled={actionBusy || writeBlocked}
              onClick={onRequestSync}
              data-testid="instruction-sync-to-native"
            >
              {labels.syncToNative}
            </Button>
          ) : null}
          {showAdaptToOthers ? (
            <Button
              variant="secondary"
              size="sm"
              loading={busyAction === 'adapt'}
              disabled={actionBusy || writeBlocked}
              onClick={onAdaptToOtherAgents}
              data-testid="instruction-adapt-to-other-agents"
            >
              {labels.adaptToOtherAgents}
            </Button>
          ) : null}
        </div>
      </div>

      {(showAdaptToOthers || showWriteToNative) && writeBlocked && writeBlockedReason ? (
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

      {aiReviseFeedback ? (
        <StatusMessage tone="success" data-testid="instruction-ai-revise-success">
          {aiReviseFeedback.currentSlotChanged
            ? labels.aiReviseSavedAndLocated
            : aiReviseFeedback.otherAdaptedSlotsChanged
              ? labels.aiReviseSavedOtherAgents
              : labels.aiReviseSavedNoChange}
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
  textareaRef: RefObject<HTMLTextAreaElement | null>;
  onSlotTextChange: (text: string) => void;
}): JSX.Element {
  const { labels, slotText, textareaRef, onSlotTextChange } = props;
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
            ref={textareaRef}
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
  textareaRef: RefObject<HTMLTextAreaElement | null>;
  onSlotTextChange: (text: string) => void;
}): JSX.Element {
  const { labels, slotText, textareaRef, onSlotTextChange } = props;
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
            ref={textareaRef}
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
 * Code Logic: 原始栏「分析拆解」；工具栏「写入原始」由 chrome 承接。
 */
function ExclusiveLanePanes(props: {
  labels: InstructionThreePaneViewLabels;
  state: InstructionThreePaneState;
  slotText: string;
  textareaRef: RefObject<HTMLTextAreaElement | null>;
  onSlotTextChange: (text: string) => void;
  onAnalyzeDecompose: () => void;
  analyzeDisabled: boolean;
  analyzeLoading: boolean;
}): JSX.Element {
  const {
    labels,
    state,
    slotText,
    textareaRef,
    onSlotTextChange,
    onAnalyzeDecompose,
    analyzeDisabled,
    analyzeLoading,
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
            ref={textareaRef}
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
            loading={analyzeLoading}
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
    busyAction,
    writeBlocked,
    writeBlockedReason,
    dualDirtyOpen,
    analyzeConfirmOpen,
    aiReviseOpen,
    aiReviseDirection,
    aiReviseError,
    aiReviseFeedback,
    aiReviseDisabled,
    onAnalyzeDecompose,
    onAdaptToOtherAgents,
    onSaveBlocks,
    onOpenAiRevise,
    onAiReviseDirectionChange,
    onCancelAiRevise,
    onConfirmAiRevise,
    onRequestSync,
    onRetry,
    onDiscardAndReload,
    onSlotTextChange,
    onChooseBaseline,
    onCancelDualDirty,
    onConfirmAnalyze,
    onCancelAnalyze,
    slotHistoryOpen,
    slotHistoryLoading,
    slotHistoryError,
    slotHistoryActionError,
    restoringSlotVersionId,
    slotHistoryVersions,
    onOpenSlotHistory,
    onCloseSlotHistory,
    onCopySlotVersion,
    onRestoreSlotVersion,
  } = props;
  const slotTextareaRef = useRef<HTMLTextAreaElement | null>(null);

  /**
   * Business Logic: AI 保存成功后直接把用户带到当前可见槽的首处变化，长提示词无需人工寻找。
   * Code Logic: controller 提供 DOM UTF-16 选区；聚焦 textarea 后设置选区，浏览器负责滚动到光标。
   */
  useEffect(() => {
    const selection = aiReviseFeedback?.selection;
    const textarea = slotTextareaRef.current;
    if (!selection || !textarea) return;
    textarea.focus({ preventScroll: true });
    textarea.setSelectionRange(selection.start, selection.end, 'forward');
  }, [aiReviseFeedback]);

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
  /** 合成预览 → 写原始仅在独有三列上下文有意义。 */
  const showWriteToNative = instructionLane === 'exclusive';

  return (
    <div className={styles.root} data-testid="instruction-three-pane">
      <InstructionChrome
        labels={labels}
        state={state}
        actionBusy={actionBusy}
        busyAction={busyAction}
        writeBlocked={writeBlocked}
        writeBlockedReason={writeBlockedReason}
        actionError={actionError}
        aiReviseFeedback={aiReviseFeedback}
        refreshError={error}
        dualDirtyOpen={dualDirtyOpen}
        showPath={showPath}
        showAdaptToOthers={showAdaptToOthers}
        showWriteToNative={showWriteToNative}
        aiReviseDisabled={aiReviseDisabled}
        instructionLane={instructionLane}
        onRetry={onRetry}
        onDiscardAndReload={onDiscardAndReload}
        onSaveBlocks={onSaveBlocks}
        onOpenAiRevise={onOpenAiRevise}
        onAdaptToOtherAgents={onAdaptToOtherAgents}
        onRequestSync={onRequestSync}
        onChooseBaseline={onChooseBaseline}
        onCancelDualDirty={onCancelDualDirty}
        onOpenSlotHistory={onOpenSlotHistory}
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
          textareaRef={slotTextareaRef}
          onSlotTextChange={onSlotTextChange}
        />
      ) : null}

      {instructionLane === 'adapted' ? (
        <AdaptedLanePanes
          labels={labels}
          slotText={adaptedSlotText}
          textareaRef={slotTextareaRef}
          onSlotTextChange={onSlotTextChange}
        />
      ) : null}

      {instructionLane === 'exclusive' ? (
        <ExclusiveLanePanes
          labels={labels}
          state={state}
          slotText={exclusiveSlotText}
          textareaRef={slotTextareaRef}
          onSlotTextChange={onSlotTextChange}
          onAnalyzeDecompose={onAnalyzeDecompose}
          analyzeLoading={busyAction === 'analyze'}
          analyzeDisabled={
            actionBusy ||
            state.sourceDrift ||
            state.externalDrift ||
            state.originalText.trim().length === 0
          }
        />
      ) : null}

      <AiReviseInstructionDialog
        open={aiReviseOpen}
        title={labels.aiReviseTitle}
        description={
          instructionLane === 'common'
            ? labels.aiReviseDescriptionCommon
            : instructionLane === 'adapted'
              ? labels.aiReviseDescriptionAdapted
              : labels.aiReviseDescriptionExclusive
        }
        directionLabel={labels.aiReviseDirectionLabel}
        directionPlaceholder={labels.aiReviseDirectionPlaceholder}
        confirmLabel={labels.aiReviseConfirm}
        cancelLabel={labels.cancel}
        direction={aiReviseDirection}
        error={aiReviseError}
        busy={busyAction === 'revise'}
        onDirectionChange={onAiReviseDirectionChange}
        onCancel={onCancelAiRevise}
        onConfirm={onConfirmAiRevise}
      />

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

      {slotHistoryActionError ? (
        <StatusMessage
          tone="danger"
          data-testid="instruction-slot-history-action-error"
        >
          {slotHistoryActionError}
        </StatusMessage>
      ) : null}

      <VersionHistoryDrawer
        open={slotHistoryOpen}
        onClose={onCloseSlotHistory}
        versions={slotHistoryVersions}
        loading={slotHistoryLoading}
        error={slotHistoryError}
        restoringVersionId={restoringSlotVersionId}
        i18nNamespace="agentHub"
        onRestore={onRestoreSlotVersion}
        onCopy={onCopySlotVersion}
      />
    </div>
  );
}
