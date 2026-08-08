/**
 * InstructionBlocksDrawer — 指令块编辑侧栏。
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要查看/调整 shared|adapted|targetOnly 块，并在确认前预览受影响 target 差异。
 *
 * Code Logic（这个组件做什么）:
 *   pure props 视图：展示 blocks、mode 操作与 diff preview；不 import @/api/*。
 */

import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Drawer, Pill, StatusMessage } from '@/components/primitives';
import type {
  AgentHubAssetDetail,
  AgentTarget,
  InstructionBlockDto,
  InstructionBlockMode,
} from '@/lib/types/agentHub';
import styles from './AgentHub.module.css';

const TARGETS: AgentTarget[] = ['claude', 'codex', 'opencode'];

export interface InstructionBlocksDrawerProps {
  open: boolean;
  asset: AgentHubAssetDetail | null;
  busy?: boolean;
  writeBlocked?: boolean;
  error?: string | null;
  onClose: () => void;
  onSaveDocument: (contentMarkdown: string) => void;
  onPromoteShared: (blockId: string, commonMarkdown: string) => void;
  onPairAdapted: (blockIds: string[], commonMarkdown?: string) => void;
  onRevertTargetOnly: (blockId: string, sourceTarget: AgentTarget, markdown: string) => void;
  onUpdateBlock: (
    blockId: string,
    patch: {
      mode?: InstructionBlockMode;
      commonMarkdown?: string;
      variants?: Partial<Record<AgentTarget, string>> | null;
    },
  ) => void;
}

/**
 * Business Logic: 预览某块在各 target 的最终文本差异。
 * Code Logic: shared 用 common；adapted 优先 variant；targetOnly 仅 source。
 */
function resolveBlockText(block: InstructionBlockDto, target: AgentTarget): string | null {
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
 * 指令块 Drawer 视图。
 */
export function InstructionBlocksDrawer({
  open,
  asset,
  busy = false,
  writeBlocked = false,
  error = null,
  onClose,
  onSaveDocument,
  onPromoteShared,
  onPairAdapted,
  onRevertTargetOnly,
  onUpdateBlock,
}: InstructionBlocksDrawerProps) {
  const { t } = useTranslation(['agentHub', 'common']);
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const [confirmPreview, setConfirmPreview] = useState(false);
  const [documentDraft, setDocumentDraft] = useState('');
  // Safe-save 合同：用户每次编辑递增 version，submit 捕获快照；
  // success 后仅在 version 未变时回填 baseline，busy 时阻止重入。
  const documentEditVersionRef = useRef(0);
  const documentSubmittedSnapshotRef = useRef<string | null>(null);
  // 资产切换或后端返回新 markdown 时，把 draft 同步到最新 baseline，
  // 覆盖未提交编辑（用户已点保存即触发 onSaveDocument，prop 变更意味着后端已接受）。
  useEffect(() => {
    // Sync draft to latest baseline when asset identity/content changes (user already saved).
    // eslint-disable-next-line react-hooks/set-state-in-effect -- prop→draft resync contract
    setDocumentDraft(asset?.contentMarkdown ?? '');
    documentSubmittedSnapshotRef.current = null;
  }, [asset?.assetId, asset?.contentMarkdown]);
  const documentBaseline = asset?.contentMarkdown ?? '';
  const documentDirty = documentDraft !== documentBaseline;
  const documentSaveDisabled = busy || writeBlocked || !documentDirty;
  function handleDocumentSave() {
    if (documentSaveDisabled) return;
    documentEditVersionRef.current += 1;
    documentSubmittedSnapshotRef.current = documentDraft;
    onSaveDocument(documentDraft);
  }

  const blocks = asset?.blocks ?? [];

  const diffPreview = useMemo(() => {
    return TARGETS.map((target) => {
      const parts = blocks
        .map((block) => {
          const text = resolveBlockText(block, target);
          if (text == null) return null;
          return `### ${block.id} (${block.mode})\n${text}`;
        })
        .filter(Boolean);
      return {
        target,
        content: parts.join('\n\n') || t('agentHub:blocks.emptyTargetFile'),
      };
    });
  }, [blocks, t]);

  /**
   * Business Logic: 切换块勾选供 pair adapted。
   * Code Logic: toggle id in selectedIds。
   */
  function toggleSelected(blockId: string) {
    setSelectedIds((prev) =>
      prev.includes(blockId) ? prev.filter((id) => id !== blockId) : [...prev, blockId],
    );
  }

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

        {blocks.length === 0 ? (
          <p className={styles.emptyInline} data-testid="blocks-empty">
            {t('agentHub:blocks.empty')}
          </p>
        ) : (
          <ul className={styles.blockList}>
            {blocks.map((block) => (
              <li key={block.id} className={styles.blockItem} data-testid={`block-item-${block.id}`}>
                <div className={styles.blockTitleRow}>
                  <label className={styles.blockCheck}>
                    <input
                      type="checkbox"
                      checked={selectedIds.includes(block.id)}
                      onChange={() => toggleSelected(block.id)}
                      disabled={busy || writeBlocked}
                    />
                    <span className={styles.blockId}>{block.id}</span>
                  </label>
                  <Pill tone="accent">{t(`agentHub:blocks.mode.${block.mode}`)}</Pill>
                  {block.needsAdaptation ? (
                    <Pill tone="warn">{t('agentHub:blocks.needsAdaptation')}</Pill>
                  ) : null}
                </div>
                {block.headingPath && block.headingPath.length > 0 ? (
                  <div className={styles.blockPath}>{block.headingPath.join(' / ')}</div>
                ) : null}
                <pre className={styles.blockBody}>{block.commonMarkdown || '—'}</pre>
                {block.mode === 'adapted' || block.mode === 'targetOnly' ? (
                  <div className={styles.variantGrid}>
                    {TARGETS.map((target) => {
                      const text = block.variants?.[target];
                      if (!text && block.mode === 'targetOnly' && block.sourceTarget !== target) {
                        return null;
                      }
                      return (
                        <div key={target} className={styles.variantCell}>
                          <div className={styles.variantLabel}>{t(`agentHub:targets.${target}`)}</div>
                          <pre className={styles.blockBody}>{text || block.commonMarkdown || '—'}</pre>
                        </div>
                      );
                    })}
                  </div>
                ) : null}
                <div className={styles.blockActions}>
                  {block.mode === 'targetOnly' ? (
                    <Button
                      size="sm"
                      variant="secondary"
                      disabled={busy || writeBlocked}
                      onClick={() => {
                        setConfirmPreview(true);
                        onPromoteShared(block.id, block.commonMarkdown);
                      }}
                      data-testid={`block-promote-${block.id}`}
                    >
                      {t('agentHub:blocks.promoteShared')}
                    </Button>
                  ) : null}
                  {block.mode !== 'targetOnly' ? (
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={busy || writeBlocked || !block.sourceTarget}
                      onClick={() => {
                        const source = block.sourceTarget ?? 'claude';
                        setConfirmPreview(true);
                        onRevertTargetOnly(
                          block.id,
                          source,
                          block.variants?.[source] ?? block.commonMarkdown,
                        );
                      }}
                      data-testid={`block-revert-${block.id}`}
                    >
                      {t('agentHub:blocks.revertTargetOnly')}
                    </Button>
                  ) : null}
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy || writeBlocked}
                    onClick={() =>
                      onUpdateBlock(block.id, {
                        mode: block.mode,
                        commonMarkdown: block.commonMarkdown,
                      })
                    }
                    data-testid={`block-save-${block.id}`}
                  >
                    {t('agentHub:blocks.saveBlock')}
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        )}

        <div className={styles.pairRow}>
          <Button
            variant="primary"
            size="sm"
            disabled={busy || writeBlocked || selectedIds.length < 2}
            onClick={() => {
              setConfirmPreview(true);
              onPairAdapted(selectedIds);
            }}
            data-testid="blocks-pair-adapted"
          >
            {t('agentHub:blocks.pairAdapted')}
          </Button>
        </div>

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
