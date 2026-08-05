import type { JSX } from 'react';
import { Button, Card, Pill, StatusMessage } from '@/components/primitives';
import type { AgentTarget, UserInstructionDraft } from '@/lib/types/agentHub';
import type { TFunction } from 'i18next';
import type { UserInstructionEditorPane } from './useUserInstructionManager';
import styles from '../AgentHub.module.css';

export interface UserInstructionEditorProps {
  t: TFunction<['agentHub', 'common']>;
  draft: UserInstructionDraft;
  activePane: UserInstructionEditorPane;
  dirty: boolean;
  busy: boolean;
  contentTruncated: boolean;
  writeAvailable: boolean;
  onPaneChange: (pane: UserInstructionEditorPane) => void;
  onContentChange: (pane: UserInstructionEditorPane, value: string) => void;
  onReset: () => void;
  onPreview: () => void;
}

const PANES: UserInstructionEditorPane[] = ['common', 'claude', 'codex', 'opencode'];

/**
 * Business Logic（为什么需要）:
 *   用户以“公共规则 + 三 Agent 专属补充”编辑 canonical，而不是理解 block map/policy。
 *
 * Code Logic（做什么）:
 *   可访问 tablist 切换四个 Markdown 草稿面；所有编辑仅回调 controller，不直接 mutation。
 */
export function UserInstructionEditor(props: UserInstructionEditorProps): JSX.Element {
  const {
    t,
    draft,
    activePane,
    dirty,
    busy,
    contentTruncated,
    writeAvailable,
    onPaneChange,
    onContentChange,
    onReset,
    onPreview,
  } = props;
  const content =
    activePane === 'common' ? draft.commonContent : (draft.targetExtensions[activePane] ?? '');
  const paneLabel = t(`agentHub:userInstructions.editor.panes.${activePane}`);

  return (
    <Card variant="outlined" padding="none" data-testid="user-instruction-editor">
      <Card.Header padding="md" className={styles.userEditorHeader}>
        <div>
          <h2 className={styles.userSectionTitle}>{t('agentHub:userInstructions.editor.title')}</h2>
          <p className={styles.userSectionDescription}>
            {t('agentHub:userInstructions.editor.description')}
          </p>
        </div>
        <div className={styles.userEditorMeta}>
          {dirty ? <Pill tone="warn">{t('agentHub:userInstructions.editor.unsaved')}</Pill> : null}
          <span>{t('agentHub:userInstructions.editor.characterCount', { count: content.length })}</span>
        </div>
      </Card.Header>
      <Card.Body padding="md" className={styles.userEditorBody}>
        {contentTruncated ? (
          <StatusMessage tone="warn" data-testid="user-instruction-content-truncated">
            {t('agentHub:userInstructions.editor.contentTruncated')}
          </StatusMessage>
        ) : null}
        {!writeAvailable ? (
          <StatusMessage tone="info" live="off" data-testid="user-instruction-editor-read-only">
            {t('agentHub:userInstructions.editor.readOnly')}
          </StatusMessage>
        ) : null}
        <div
          className={styles.userEditorTabs}
          role="tablist"
          aria-label={t('agentHub:userInstructions.editor.tabsAria')}
        >
          {PANES.map((pane) => (
            <Button
              key={pane}
              role="tab"
              variant={pane === activePane ? 'secondary' : 'ghost'}
              size="sm"
              aria-selected={pane === activePane}
              tabIndex={pane === activePane ? 0 : -1}
              onClick={() => onPaneChange(pane)}
              data-testid={`user-instruction-pane-${pane}`}
            >
              {t(`agentHub:userInstructions.editor.panes.${pane}`)}
            </Button>
          ))}
        </div>
        <div
          role="tabpanel"
          aria-label={paneLabel}
          className={styles.userEditorPanel}
          data-testid={`user-instruction-editor-${activePane}`}
        >
          <textarea
            className={styles.userInstructionTextarea}
            value={content}
            readOnly={contentTruncated}
            aria-label={t('agentHub:userInstructions.editor.textareaAria', { pane: paneLabel })}
            placeholder={t(`agentHub:userInstructions.editor.placeholders.${activePane}`)}
            onChange={(event) => onContentChange(activePane, event.currentTarget.value)}
          />
        </div>
      </Card.Body>
      <Card.Footer padding="md" className={styles.userEditorFooter}>
        <Button variant="ghost" size="sm" disabled={!dirty || busy} onClick={onReset}>
          {t('agentHub:userInstructions.editor.discard')}
        </Button>
        <Button
          variant="primary"
          size="sm"
          loading={busy}
          disabled={contentTruncated || !writeAvailable}
          onClick={onPreview}
          data-testid="user-instruction-preview-draft"
        >
          {t('agentHub:userInstructions.editor.previewApply')}
        </Button>
      </Card.Footer>
    </Card>
  );
}

export type { AgentTarget };
