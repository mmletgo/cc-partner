import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Dialog } from '@/components/primitives';
import type { MobileAutomationCreateDialogProps } from '../controllers/useMobileAutomationController';
import styles from '../MobileWorkbench.module.css';

/**
 * MobileAutomationCreateDialog（移动端自动化创建任务弹窗）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端创建任务必须是独立 Dialog，支持短 Prompt AI 完善与 Backlog/Todo/Start 三种显式动作。
 *
 * Code Logic（这个组件做什么）:
 *   纯展示：用共享 Dialog 渲染表单与动作按钮；busy 时禁用 Escape/backdrop 关闭。
 *   不导入 transport/API；所有状态与请求由 controller 提供。
 */
export function MobileAutomationCreateDialog({
  open,
  dialogTitleId,
  promptDraftRef,
  creating,
  completingPrompt,
  creatingAction,
  promptDraft,
  title,
  goal,
  acceptanceCriteria,
  canCompletePrompt,
  canSubmit,
  createActions,
  onClose,
  onPromptDraftChange,
  onTitleChange,
  onGoalChange,
  onAcceptanceCriteriaChange,
  onCompletePrompt,
  onCreateTask,
}: MobileAutomationCreateDialogProps): ReactElement {
  const { t } = useTranslation(['workbench', 'orchestrator']);

  return (
    <Dialog
      open={open}
      titleId={dialogTitleId}
      onClose={onClose}
      closeOnEscape={!(creating || completingPrompt)}
      closeOnBackdrop={!(creating || completingPrompt)}
      initialFocusRef={promptDraftRef}
      className={styles.mobileDialog}
    >
      <div className={styles.mobileDialogHeader}>
        <div className={styles.panelHeader}>
          <p className={styles.panelKicker}>{t('workbench:mobile.kicker')}</p>
          <h2 id={dialogTitleId}>{t('workbench:mobile.automationPanel.createOpen')}</h2>
        </div>
        <button
          type="button"
          className={styles.secondaryButton}
          disabled={creating || completingPrompt}
          onClick={onClose}
        >
          {t('workbench:mobile.automationPanel.closeCreate')}
        </button>
      </div>

      <div className={styles.mobileDialogBody}>
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

        <form
          className={styles.mobileFormInline}
          onSubmit={(event) => {
            event.preventDefault();
          }}
        >
          <label className={styles.mobileField}>
            <span>{t('workbench:mobile.automationPanel.fields.title')}</span>
            <input
              className={styles.mobileInput}
              value={title}
              disabled={creating || completingPrompt}
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
              disabled={creating || completingPrompt}
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
              disabled={creating || completingPrompt}
              placeholder={t(
                'workbench:mobile.automationPanel.placeholders.acceptanceCriteria',
              )}
              onChange={(event) => {
                onAcceptanceCriteriaChange(event.target.value);
              }}
            />
          </label>

          <div className={styles.mobileAutomationCreateActions}>
            {createActions.map((action) => (
              <button
                key={action.createAction}
                type="button"
                className={
                  action.createAction === 'start'
                    ? styles.mobileTerminalPrimaryButton
                    : styles.secondaryButton
                }
                disabled={!canSubmit}
                onClick={() => {
                  onCreateTask(action.createAction, action.statusKey);
                }}
              >
                {creatingAction === action.createAction
                  ? t('workbench:mobile.automationPanel.creating')
                  : t(action.labelKey)}
              </button>
            ))}
          </div>
        </form>
      </div>
    </Dialog>
  );
}

export type { MobileAutomationCreateDialogProps };
