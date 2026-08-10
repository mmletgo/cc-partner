/**
 * InstructionBlocksDrawer — 指令资产固定三槽编辑侧栏。
 *
 * Business Logic（为什么需要）:
 *   Legacy matrix 打开指令资产时，编辑面与主路径一致：公共 / 适配 / 独有，每 mode 最多一块。
 *   仍可编辑整篇正文；块侧只展示三槽，不再 promote/pair 多块操作。
 *
 * Code Logic（做什么）:
 *   pure view；normalizeInstructionBlocks 后按三槽展示；保存整篇或单槽经 props 回调。
 */

import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Drawer, Pill, StatusMessage } from '@/components/primitives';
import type {
  AgentHubAssetDetail,
  AgentTarget,
  InstructionBlockMode,
} from '@/lib/types/agentHub';
import {
  draftToDto,
  dtoToDraft,
  ensureModeBlock,
  findBlockByMode,
  joinBlocksForTarget,
  normalizeInstructionBlocks,
  updateBlock,
  type InstructionBlockDraft,
  type InstructionThreePaneState,
} from './instructions/instructionThreePane';
import styles from './AgentHub.module.css';

const TARGETS: AgentTarget[] = ['claude', 'codex', 'opencode'];
const SLOTS: Array<{
  mode: InstructionBlockMode;
  lane: 'common' | 'adapted' | 'exclusive';
}> = [
  { mode: 'shared', lane: 'common' },
  { mode: 'adapted', lane: 'adapted' },
  { mode: 'targetOnly', lane: 'exclusive' },
];

export interface InstructionBlocksDrawerProps {
  open: boolean;
  asset: AgentHubAssetDetail | null;
  busy?: boolean;
  writeBlocked?: boolean;
  error?: string | null;
  onClose: () => void;
  onSaveDocument: (contentMarkdown: string) => void;
  onUpdateBlock: (
    blockId: string,
    patch: {
      mode?: InstructionBlockMode;
      commonMarkdown?: string;
      variants?: Partial<Record<AgentTarget, string>> | null;
    },
  ) => void;
  /**
   * 兼容旧 props（promote/pair/revert）；三槽 UI 不再暴露这些动作，保留可选避免调用方断裂。
   */
  onPromoteShared?: (blockId: string, commonMarkdown: string) => void;
  onPairAdapted?: (blockIds: string[], commonMarkdown?: string) => void;
  onRevertTargetOnly?: (blockId: string, sourceTarget: AgentTarget, markdown: string) => void;
}

/**
 * Business Logic: 预览某块在各 target 的最终文本差异。
 * Code Logic: shared 用 common；adapted 优先 variant；targetOnly 仅 source。
 */
function resolveDtoBlockText(
  block: ReturnType<typeof draftToDto>,
  target: AgentTarget,
): string | null {
  if (block.mode === 'shared') {
    return block.commonMarkdown;
  }
  if (block.mode === 'adapted') {
    return block.variants?.[target] ?? block.commonMarkdown;
  }
  if (block.mode === 'targetOnly') {
    if (block.sourceTarget && block.sourceTarget !== target) return null;
    return block.variants?.[target] ?? block.commonMarkdown;
  }
  return block.commonMarkdown;
}

/**
 * 指令块 Drawer 视图（固定三槽）。
 */
export function InstructionBlocksDrawer({
  open,
  asset,
  busy = false,
  writeBlocked = false,
  error = null,
  onClose,
  onSaveDocument,
  onUpdateBlock,
}: InstructionBlocksDrawerProps) {
  const { t } = useTranslation(['agentHub', 'common']);
  const [confirmPreview, setConfirmPreview] = useState(false);
  const [documentDraft, setDocumentDraft] = useState('');
  const [activeTarget, setActiveTarget] = useState<AgentTarget>('claude');
  const [slotDrafts, setSlotDrafts] = useState<InstructionBlockDraft[]>([]);
  // Safe-save 合同：用户每次编辑递增 version，submit 捕获快照；
  // success 后仅在 version 未变时回填 baseline，busy 时阻止重入。
  const documentEditVersionRef = useRef(0);
  const documentSubmittedSnapshotRef = useRef<string | null>(null);

  useEffect(() => {
    // Sync draft to latest baseline when asset identity/content changes.
    // eslint-disable-next-line react-hooks/set-state-in-effect -- prop→draft resync contract
    setDocumentDraft(asset?.contentMarkdown ?? '');
    documentSubmittedSnapshotRef.current = null;
    const drafts = normalizeInstructionBlocks((asset?.blocks ?? []).map(dtoToDraft));
    setSlotDrafts(drafts);
    setConfirmPreview(false);
  }, [asset?.assetId, asset?.contentMarkdown, asset?.blocks]);

  const documentBaseline = asset?.contentMarkdown ?? '';
  const documentDirty = documentDraft !== documentBaseline;
  const documentSaveDisabled = busy || writeBlocked || !documentDirty;

  function handleDocumentSave() {
    if (documentSaveDisabled) return;
    documentEditVersionRef.current += 1;
    documentSubmittedSnapshotRef.current = documentDraft;
    onSaveDocument(documentDraft);
  }

  /**
   * Business Logic: 编辑某一槽正文并即时写回该 mode 块。
   * Code Logic: ensure mode → updateBlock 本地 → onUpdateBlock 持久化。
   */
  function handleSlotEdit(mode: InstructionBlockMode, text: string) {
    let nextState: InstructionThreePaneState = {
      originalPath: null,
      originalText: '',
      blocks: slotDrafts,
      previewText: '',
      blocksDirty: false,
      originalDirty: false,
      externalDrift: false,
      sourceDrift: false,
    };
    nextState = ensureModeBlock(nextState, mode, activeTarget);
    const block = findBlockByMode(nextState.blocks, mode);
    if (!block) return;
    if (mode === 'shared') {
      nextState = updateBlock(nextState, block.id, { commonMarkdown: text }, activeTarget);
    } else {
      nextState = updateBlock(
        nextState,
        block.id,
        {
          variants: { ...block.variants, [activeTarget]: text },
          sourceTarget: mode === 'targetOnly' ? (block.sourceTarget ?? activeTarget) : null,
        },
        activeTarget,
      );
    }
    const nextBlocks = normalizeInstructionBlocks(nextState.blocks);
    setSlotDrafts(nextBlocks);
    const updated = findBlockByMode(nextBlocks, mode);
    if (!updated) return;
    onUpdateBlock(updated.id, {
      mode: updated.mode,
      commonMarkdown: updated.commonMarkdown,
      variants: Object.keys(updated.variants).length > 0 ? updated.variants : null,
    });
    setConfirmPreview(true);
  }

  const diffPreview = useMemo(() => {
    const dtoBlocks = slotDrafts.map(draftToDto);
    return TARGETS.map((target) => {
      const parts = dtoBlocks
        .map((block) => {
          const text = resolveDtoBlockText(block, target);
          if (text == null || text.trim().length === 0) return null;
          return `### ${block.mode}\n${text}`;
        })
        .filter(Boolean);
      return {
        target,
        content: parts.join('\n\n') || t('agentHub:blocks.emptyTargetFile'),
      };
    });
  }, [slotDrafts, t]);

  const composedForActive = useMemo(
    () => joinBlocksForTarget(slotDrafts, activeTarget),
    [slotDrafts, activeTarget],
  );

  return (
    <Drawer
      open={open}
      titleId="agent-hub-blocks-title"
      onClose={onClose}
      side="right"
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
      className={styles.drawerSurface}
    >
      <div className={styles.drawerBody} data-testid="instruction-blocks-drawer">
        <header className={styles.drawerHeader}>
          <h2 id="agent-hub-blocks-title" className={styles.drawerTitle}>
            {t('agentHub:blocks.title')}
          </h2>
          <p className={styles.drawerSubtitle}>
            {asset ? asset.displayName : t('agentHub:blocks.noAsset')}
          </p>
          <p className={styles.hint}>{t('agentHub:blocks.threeSlotHint')}</p>
        </header>

        {error ? (
          <StatusMessage tone="danger" data-testid="blocks-drawer-error">
            {error}
          </StatusMessage>
        ) : null}

        {writeBlocked ? (
          <StatusMessage tone="warn" data-testid="blocks-upgrade-required">
            {t('agentHub:upgradeRequired')}
          </StatusMessage>
        ) : null}

        <section className={styles.documentSection} data-testid="instruction-document-section">
          <header className={styles.documentHeader}>
            <h3 className={styles.sectionTitle}>{t('agentHub:documentEditor.title')}</h3>
            <p className={styles.hint}>{t('agentHub:documentEditor.hint')}</p>
          </header>
          <textarea
            className={styles.documentTextarea}
            data-testid="instruction-document-editor"
            value={documentDraft}
            onChange={(event) => setDocumentDraft(event.currentTarget.value)}
            disabled={busy || writeBlocked}
            rows={12}
            aria-label={t('agentHub:documentEditor.ariaLabel')}
          />
          <div className={styles.documentActions}>
            <Button
              size="sm"
              variant="primary"
              disabled={documentSaveDisabled}
              loading={busy}
              onClick={handleDocumentSave}
              data-testid="instruction-document-save"
            >
              {t('agentHub:documentEditor.save')}
            </Button>
            {documentDirty && !busy ? (
              <Pill tone="accent">{t('agentHub:documentEditor.unsaved')}</Pill>
            ) : null}
          </div>
        </section>

        <section className={styles.documentSection} data-testid="instruction-three-slots">
          <header className={styles.documentHeader}>
            <h3 className={styles.sectionTitle}>{t('agentHub:blocks.threeSlotsTitle')}</h3>
            <div
              className={styles.segment}
              role="tablist"
              aria-label={t('agentHub:shell.agentAria')}
              data-testid="blocks-drawer-agent-switcher"
            >
              {TARGETS.map((target) => (
                <Button
                  key={target}
                  size="sm"
                  variant={activeTarget === target ? 'secondary' : 'ghost'}
                  role="tab"
                  aria-selected={activeTarget === target}
                  onClick={() => setActiveTarget(target)}
                  data-testid={`blocks-drawer-agent-${target}`}
                >
                  {t(`agentHub:targets.${target}`)}
                </Button>
              ))}
            </div>
          </header>

          <ul className={styles.blockList}>
            {SLOTS.map(({ mode, lane }) => {
              const block = findBlockByMode(slotDrafts, mode);
              const text =
                mode === 'shared'
                  ? (block?.commonMarkdown ?? '')
                  : mode === 'adapted'
                    ? (block?.variants[activeTarget] ?? block?.commonMarkdown ?? '')
                    : (block?.variants[activeTarget] ?? '');
              return (
                <li
                  key={mode}
                  className={styles.blockItem}
                  data-testid={`block-slot-${lane}`}
                >
                  <div className={styles.blockTitleRow}>
                    <span className={styles.blockId}>{t(`agentHub:shell.lanes.${lane}`)}</span>
                    <Pill tone="accent">{t(`agentHub:blocks.mode.${mode}`)}</Pill>
                  </div>
                  <p className={styles.hint}>
                    {lane === 'common'
                      ? t('agentHub:instructions.threePane.slotCommonHint')
                      : lane === 'adapted'
                        ? t('agentHub:instructions.threePane.slotAdaptedHint')
                        : t('agentHub:instructions.threePane.slotExclusiveHint')}
                  </p>
                  <textarea
                    className={styles.documentTextarea}
                    data-testid={`block-slot-text-${lane}`}
                    value={text}
                    rows={6}
                    disabled={busy || writeBlocked}
                    onChange={(event) => handleSlotEdit(mode, event.currentTarget.value)}
                    aria-label={t(`agentHub:shell.lanes.${lane}`)}
                  />
                </li>
              );
            })}
          </ul>

          <div className={styles.documentSection} data-testid="blocks-composed-preview">
            <h3 className={styles.sectionTitle}>
              {t('agentHub:instructions.threePane.previewTitle')} · {t(`agentHub:targets.${activeTarget}`)}
            </h3>
            <pre className={styles.blockBody}>
              {composedForActive.trim().length > 0
                ? composedForActive
                : t('agentHub:instructions.threePane.emptyPreview')}
            </pre>
          </div>
        </section>

        <section className={styles.diffSection} data-testid="blocks-diff-preview">
          <h3 className={styles.sectionTitle}>{t('agentHub:blocks.diffPreview')}</h3>
          {!confirmPreview ? (
            <p className={styles.hint}>{t('agentHub:blocks.diffHint')}</p>
          ) : null}
          <div className={styles.diffGrid}>
            {diffPreview.map((item) => (
              <div key={item.target} className={styles.diffCell} data-testid={`diff-${item.target}`}>
                <div className={styles.variantLabel}>{t(`agentHub:targets.${item.target}`)}</div>
                <pre className={styles.blockBody}>{item.content}</pre>
              </div>
            ))}
          </div>
        </section>
      </div>
    </Drawer>
  );
}
