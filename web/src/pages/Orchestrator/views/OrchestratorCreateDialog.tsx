/**
 * Orchestrator 创建任务弹窗视图
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要通过独立 Dialog 填写任务三字段或用 AI 完善后，显式选择创建到 Backlog/Todo 或创建并启动。
 *
 * Code Logic（这个组件做什么）:
 *   渲染共享 Dialog + 表单 + AI 完善 + 三个 createAction 按钮；busy 时禁用关闭；无 API import。
 */
import type { FormEvent, JSX, RefObject } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Dialog, Input } from '@/components/primitives';
import { PlusIcon, SyncIcon, XIcon } from '@/lib/icons';
import type { OrchestratorCreateAction, OrchestratorCreateForm } from '../orchestratorViewHelpers';
import { ORCHESTRATOR_CREATE_ACTIONS } from '../orchestratorViewHelpers';
import styles from '../Orchestrator.module.css';

/**
 * Business Logic（为什么需要这个类型）:
 *   创建弹窗状态与提交逻辑归 controller，视图只绑定受控字段与回调。
 *
 * Code Logic（这个类型做什么）:
 *   描述 open/busy/form/prompt 与 close/complete/create 回调。
 */
export interface OrchestratorCreateDialogProps {
  open: boolean;
  form: OrchestratorCreateForm;
  completionPrompt: string;
  completingPrompt: boolean;
  creatingAction: OrchestratorCreateAction | null;
  canCreate: boolean;
  canCompletePrompt: boolean;
  completionPromptRef: RefObject<HTMLTextAreaElement | null>;
  creatingExperiment?: boolean;
  onClose: () => void;
  onCompletionPromptChange: (value: string) => void;
  onUpdateFormField: (field: keyof OrchestratorCreateForm, value: string) => void;
  onCompleteWithAi: () => void;
  onCreateFormSubmit: (event: FormEvent<HTMLFormElement>) => void;
  onCreateAction: (createAction: OrchestratorCreateAction) => void;
  onCreateExperiment?: () => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   创建入口必须是独立弹窗，不能占用看板主文档流。
 *
 * Code Logic（这个函数做什么）:
 *   用共享 Dialog（busy 时 closeOnEscape/closeOnBackdrop=false）渲染 AI 区、表单与三动作按钮。
 */
export function OrchestratorCreateDialog(props: OrchestratorCreateDialogProps): JSX.Element {
  const {
    open,
    form,
    completionPrompt,
    completingPrompt,
    creatingAction,
    canCreate,
    canCompletePrompt,
    completionPromptRef,
    creatingExperiment = false,
    onClose,
    onCompletionPromptChange,
    onUpdateFormField,
    onCompleteWithAi,
    onCreateFormSubmit,
    onCreateAction,
    onCreateExperiment,
  } = props;
  const { t } = useTranslation(['orchestrator', 'common']);
  const busy = Boolean(creatingAction) || completingPrompt || creatingExperiment;

  return (
    <Dialog
      open={open}
      titleId="orchestrator-create-dialog-title"
      onClose={onClose}
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
      initialFocusRef={completionPromptRef}
      className={styles.createDialog}
    >
      <Card variant="elevated" padding="md">
        <Card.Header className={styles.dialogHeader}>
          <div>
            <h2 id="orchestrator-create-dialog-title" className={styles.sectionTitle}>
              {t('orchestrator:create.title')}
            </h2>
            <p className={styles.sectionLead}>{t('orchestrator:create.subtitle')}</p>
          </div>
          <Button
            variant="icon"
            aria-label={t('orchestrator:create.close')}
            icon={<XIcon />}
            disabled={busy}
            onClick={onClose}
          />
        </Card.Header>
        <Card.Body className={styles.dialogBody}>
          <div className={styles.aiAssistBlock}>
            <label className={styles.field}>
              <span>{t('orchestrator:create.quickPrompt')}</span>
              <textarea
                ref={completionPromptRef}
                className={styles.textarea}
                value={completionPrompt}
                onChange={(event) => onCompletionPromptChange(event.target.value)}
                placeholder={t('orchestrator:create.quickPromptPlaceholder')}
                aria-label={t('orchestrator:create.quickPrompt')}
                rows={4}
                disabled={completingPrompt || Boolean(creatingAction)}
              />
            </label>
            <div className={styles.aiAssistActions}>
              <Button
                variant="secondary"
                size="sm"
                icon={<SyncIcon />}
                loading={completingPrompt}
                disabled={!canCompletePrompt || Boolean(creatingAction)}
                onClick={onCompleteWithAi}
              >
                {t('orchestrator:create.completeWithAi')}
              </Button>
            </div>
          </div>
          <form className={styles.form} onSubmit={onCreateFormSubmit}>
            <label className={styles.field}>
              <span>{t('orchestrator:create.taskTitle')}</span>
              <Input
                value={form.title}
                onChange={(event) => onUpdateFormField('title', event.target.value)}
                placeholder={t('orchestrator:create.taskTitlePlaceholder')}
                aria-label={t('orchestrator:create.taskTitle')}
                disabled={Boolean(creatingAction)}
              />
            </label>
            <label className={styles.field}>
              <span>{t('orchestrator:create.goal')}</span>
              <textarea
                className={styles.textarea}
                value={form.goal}
                onChange={(event) => onUpdateFormField('goal', event.target.value)}
                placeholder={t('orchestrator:create.goalPlaceholder')}
                aria-label={t('orchestrator:create.goal')}
                rows={5}
                disabled={Boolean(creatingAction)}
              />
            </label>
            <label className={styles.field}>
              <span>{t('orchestrator:create.acceptanceCriteria')}</span>
              <textarea
                className={styles.textarea}
                value={form.acceptanceCriteria}
                onChange={(event) => onUpdateFormField('acceptanceCriteria', event.target.value)}
                placeholder={t('orchestrator:create.acceptanceCriteriaPlaceholder')}
                aria-label={t('orchestrator:create.acceptanceCriteria')}
                rows={5}
                disabled={Boolean(creatingAction)}
              />
            </label>
            <div className={styles.dialogActions}>
              <Button variant="ghost" size="md" disabled={busy} onClick={onClose}>
                {t('common:action.cancel')}
              </Button>
              {ORCHESTRATOR_CREATE_ACTIONS.map((action) => (
                <Button
                  variant={action.variant}
                  size="md"
                  type="button"
                  icon={<PlusIcon />}
                  key={action.createAction}
                  loading={creatingAction === action.createAction}
                  disabled={!canCreate || completingPrompt || creatingExperiment}
                  onClick={() => {
                    void onCreateAction(action.createAction);
                  }}
                >
                  {t(action.labelKey)}
                </Button>
              ))}
              {onCreateExperiment ? (
                <Button
                  variant="secondary"
                  size="md"
                  type="button"
                  loading={creatingExperiment}
                  disabled={!canCreate || completingPrompt || creatingExperiment}
                  onClick={() => {
                    void onCreateExperiment();
                  }}
                  data-testid="create-experiment"
                >
                  {t('orchestrator:experiments.create')}
                </Button>
              ) : null}
            </div>
          </form>
        </Card.Body>
      </Card>
    </Dialog>
  );
}
