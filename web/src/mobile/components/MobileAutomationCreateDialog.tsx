import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Dialog } from '@/components/primitives';
import {
  MAX_ORCHESTRATOR_BLOCK_MEMBERS,
  MIN_ORCHESTRATOR_BLOCK_MEMBERS,
} from '@/pages/Orchestrator/orchestratorBoard';
import type { MobileAutomationCreateDialogProps } from '../controllers/useMobileAutomationController';
import styles from '../MobileWorkbench.module.css';

/**
 * MobileAutomationCreateDialog（移动端自动化创建任务弹窗）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端创建任务/任务块必须是独立 Dialog，支持短 Prompt AI 完善与 Backlog/Todo/Start 三种显式动作。
 *
 * Code Logic（这个组件做什么）:
 *   纯展示：用共享 Dialog 渲染模式切换、表单与动作按钮；append 模式只提交三字段。
 *   不导入 transport/API。
 */
export function MobileAutomationCreateDialog({
  open,
  dialogTitleId,
  dialogKind,
  createMode,
  preferredCreateAction,
  promptDraftRef,
  creating,
  completingPrompt,
  creatingAction,
  appending,
  promptDraft,
  title,
  goal,
  acceptanceCriteria,
  blockTitle,
  blockMembers,
  canCompletePrompt,
  canSubmit,
  canCreateBlock,
  canCreateTaskBlock,
  canAppend,
  createActions,
  onClose,
  onCreateModeChange,
  onPromptDraftChange,
  onTitleChange,
  onGoalChange,
  onAcceptanceCriteriaChange,
  onBlockTitleChange,
  onUpdateBlockMember,
  onAddBlockMember,
  onRemoveBlockMember,
  onCompletePrompt,
  onCreateTask,
  onAppendSubmit,
}: MobileAutomationCreateDialogProps): ReactElement {
  const { t } = useTranslation(['workbench', 'orchestrator']);
  const busy = creating || completingPrompt || appending;
  const isAppend = dialogKind === 'append';
  const isBlock = !isAppend && createMode === 'taskBlock';

  return (
    <Dialog
      open={open}
      titleId={dialogTitleId}
      onClose={onClose}
      closeOnEscape={!(creating || completingPrompt || appending)}
      closeOnBackdrop={!(creating || completingPrompt || appending)}
      initialFocusRef={promptDraftRef}
      className={styles.mobileDialog}
    >
      <div className={styles.mobileDialogHeader}>
        <div className={styles.panelHeader}>
          <h2 id={dialogTitleId}>
            {isAppend
              ? t('workbench:mobile.automationPanel.appendTitle')
              : t('workbench:mobile.automationPanel.createOpen')}
          </h2>
        </div>
        <button
          type="button"
          className={styles.secondaryButton}
          disabled={busy}
          onClick={onClose}
        >
          {t('workbench:mobile.automationPanel.closeCreate')}
        </button>
      </div>

      <div className={styles.mobileDialogBody}>
        {!isAppend ? (
          <div className={styles.mobileBadgeRow}>
            <button
              type="button"
              className={
                createMode === 'task'
                  ? styles.mobileTerminalPrimaryButton
                  : styles.secondaryButton
              }
              disabled={busy}
              onClick={() => onCreateModeChange('task')}
            >
              {t('workbench:mobile.automationPanel.modeTask')}
            </button>
            <button
              type="button"
              className={
                createMode === 'taskBlock'
                  ? styles.mobileTerminalPrimaryButton
                  : styles.secondaryButton
              }
              disabled={busy || !canCreateTaskBlock}
              onClick={() => onCreateModeChange('taskBlock')}
            >
              {t('workbench:mobile.automationPanel.modeBlock')}
            </button>
          </div>
        ) : null}
        {!isAppend && !canCreateTaskBlock ? (
          <p className={styles.mobileDialogAssist}>{t('orchestrator:create.unsupportedBlocks')}</p>
        ) : null}

        {!isAppend ? (
          <div className={styles.mobileDialogAssist}>
            <label className={styles.mobileField}>
              <span>{t('workbench:mobile.automationPanel.shortPrompt')}</span>
              <textarea
                ref={promptDraftRef}
                className={styles.mobileTextarea}
                value={promptDraft}
                disabled={creating || completingPrompt}
                placeholder={t('workbench:mobile.automationPanel.shortPromptPlaceholder')}
                onChange={(event) => {
                  onPromptDraftChange(event.target.value);
                }}
              />
            </label>
            <button
              type="button"
              className={styles.mobileTerminalPrimaryButton}
              disabled={!canCompletePrompt}
              onClick={onCompletePrompt}
            >
              {completingPrompt
                ? t('workbench:mobile.automationPanel.completingPrompt')
                : t('workbench:mobile.automationPanel.completeWithAi')}
            </button>
          </div>
        ) : null}

        <form
          className={styles.mobileFormInline}
          onSubmit={(event) => {
            event.preventDefault();
          }}
        >
          {isBlock ? (
            <>
              <label className={styles.mobileField}>
                <span>{t('workbench:mobile.automationPanel.blockTitle')}</span>
                <input
                  className={styles.mobileInput}
                  value={blockTitle}
                  disabled={busy}
                  onChange={(event) => onBlockTitleChange(event.target.value)}
                />
              </label>
              {blockMembers.map((member, index) => (
                <div className={styles.mobileFormInline} key={`mobile-block-member-${index}`}>
                  <div className={styles.mobileListTitleRow}>
                    <span>{t('workbench:mobile.automationPanel.memberIndex', { index: index + 1 })}</span>
                    {blockMembers.length > MIN_ORCHESTRATOR_BLOCK_MEMBERS ? (
                      <button
                        type="button"
                        className={styles.secondaryButton}
                        disabled={busy}
                        onClick={() => onRemoveBlockMember(index)}
                      >
                        {t('workbench:mobile.automationPanel.removeMember')}
                      </button>
                    ) : null}
                  </div>
                  <label className={styles.mobileField}>
                    <span>{t('workbench:mobile.automationPanel.fields.title')}</span>
                    <input
                      className={styles.mobileInput}
                      value={member.title}
                      disabled={busy}
                      onChange={(event) => onUpdateBlockMember(index, 'title', event.target.value)}
                    />
                  </label>
                  <label className={styles.mobileField}>
                    <span>{t('workbench:mobile.automationPanel.fields.goal')}</span>
                    <textarea
                      className={styles.mobileTextarea}
                      value={member.goal}
                      disabled={busy}
                      onChange={(event) => onUpdateBlockMember(index, 'goal', event.target.value)}
                    />
                  </label>
                  <label className={styles.mobileField}>
                    <span>{t('workbench:mobile.automationPanel.fields.acceptanceCriteria')}</span>
                    <textarea
                      className={styles.mobileTextarea}
                      value={member.acceptanceCriteria}
                      disabled={busy}
                      onChange={(event) =>
                        onUpdateBlockMember(index, 'acceptanceCriteria', event.target.value)
                      }
                    />
                  </label>
                </div>
              ))}
              {blockMembers.length < MAX_ORCHESTRATOR_BLOCK_MEMBERS ? (
                <button
                  type="button"
                  className={styles.secondaryButton}
                  disabled={busy}
                  onClick={onAddBlockMember}
                >
                  {t('workbench:mobile.automationPanel.addMember')}
                </button>
              ) : null}
            </>
          ) : (
            <>
              <label className={styles.mobileField}>
                <span>{t('workbench:mobile.automationPanel.fields.title')}</span>
                <input
                  className={styles.mobileInput}
                  value={title}
                  disabled={busy}
                  placeholder={t('workbench:mobile.automationPanel.placeholders.title')}
                  onChange={(event) => {
                    onTitleChange(event.target.value);
                  }}
                />
              </label>
              <label className={styles.mobileField}>
                <span>{t('workbench:mobile.automationPanel.fields.goal')}</span>
                <textarea
                  className={styles.mobileTextarea}
                  value={goal}
                  disabled={busy}
                  placeholder={t('workbench:mobile.automationPanel.placeholders.goal')}
                  onChange={(event) => {
                    onGoalChange(event.target.value);
                  }}
                />
              </label>
              <label className={styles.mobileField}>
                <span>{t('workbench:mobile.automationPanel.fields.acceptanceCriteria')}</span>
                <textarea
                  className={styles.mobileTextarea}
                  value={acceptanceCriteria}
                  disabled={busy}
                  placeholder={t(
                    'workbench:mobile.automationPanel.placeholders.acceptanceCriteria',
                  )}
                  onChange={(event) => {
                    onAcceptanceCriteriaChange(event.target.value);
                  }}
                />
              </label>
            </>
          )}

          <div className={styles.mobileAutomationCreateActions}>
            {isAppend ? (
              <button
                type="button"
                className={styles.mobileTerminalPrimaryButton}
                disabled={!canAppend}
                onClick={onAppendSubmit}
              >
                {appending
                  ? t('workbench:mobile.automationPanel.creating')
                  : t('workbench:mobile.automationPanel.appendSubmit')}
              </button>
            ) : (
              createActions.map((action) => {
                const preferred = preferredCreateAction === action.createAction;
                return (
                  <button
                    key={action.createAction}
                    type="button"
                    className={
                      preferred || action.createAction === 'start'
                        ? styles.mobileTerminalPrimaryButton
                        : styles.secondaryButton
                    }
                    disabled={isBlock ? !canCreateBlock || !canCreateTaskBlock : !canSubmit}
                    onClick={() => {
                      onCreateTask(action.createAction, action.statusKey);
                    }}
                  >
                    {creatingAction === action.createAction
                      ? t('workbench:mobile.automationPanel.creating')
                      : t(action.labelKey)}
                  </button>
                );
              })
            )}
          </div>
        </form>
      </div>
    </Dialog>
  );
}

export type { MobileAutomationCreateDialogProps };
