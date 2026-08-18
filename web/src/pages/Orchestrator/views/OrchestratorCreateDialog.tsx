/**
 * Orchestrator 创建任务弹窗视图
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要通过独立 Dialog 创建任务或任务块，或向已有块末尾追加成员；创建必须显式点 createAction。
 *
 * Code Logic（这个组件做什么）:
 *   渲染共享 Dialog + 模式切换 + 任务/块表单 + AI 完善；append 模式只提交三字段，无 createAction。
 */
import type { FormEvent, JSX, RefObject } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Dialog, Input } from '@/components/primitives';
import { PlusIcon, SyncIcon, XIcon } from '@/lib/icons';
import {
  MAX_ORCHESTRATOR_BLOCK_MEMBERS,
  MIN_ORCHESTRATOR_BLOCK_MEMBERS,
} from '../orchestratorBoard';
import type { OrchestratorCreateAction, OrchestratorCreateForm } from '../orchestratorViewHelpers';
import { ORCHESTRATOR_CREATE_ACTIONS } from '../orchestratorViewHelpers';
import type { OrchestratorCreateMode } from './OrchestratorBoard';
import styles from '../Orchestrator.module.css';

export type OrchestratorCreateDialogKind = 'create' | 'append';

/**
 * Business Logic（为什么需要这个类型）:
 *   创建弹窗状态与提交逻辑归 controller，视图只绑定受控字段与回调。
 *
 * Code Logic（这个类型做什么）:
 *   描述 open/mode/form/block/append 与 close/complete/create 回调。
 */
export interface OrchestratorCreateDialogProps {
  open: boolean;
  dialogKind: OrchestratorCreateDialogKind;
  createMode: OrchestratorCreateMode;
  preferredCreateAction: OrchestratorCreateAction | null;
  form: OrchestratorCreateForm;
  blockTitle: string;
  blockMembers: OrchestratorCreateForm[];
  completionPrompt: string;
  completingPrompt: boolean;
  creatingAction: OrchestratorCreateAction | null;
  appending: boolean;
  canCreate: boolean;
  canCreateBlock: boolean;
  canAppend: boolean;
  canCompletePrompt: boolean;
  canCreateTaskBlock: boolean;
  completionPromptRef: RefObject<HTMLTextAreaElement | null>;
  creatingExperiment?: boolean;
  onClose: () => void;
  onCreateModeChange: (mode: OrchestratorCreateMode) => void;
  onCompletionPromptChange: (value: string) => void;
  onUpdateFormField: (field: keyof OrchestratorCreateForm, value: string) => void;
  onBlockTitleChange: (value: string) => void;
  onUpdateBlockMember: (
    index: number,
    field: keyof OrchestratorCreateForm,
    value: string,
  ) => void;
  onAddBlockMember: () => void;
  onRemoveBlockMember: (index: number) => void;
  onCompleteWithAi: () => void;
  onCreateFormSubmit: (event: FormEvent<HTMLFormElement>) => void;
  onCreateAction: (createAction: OrchestratorCreateAction) => void;
  onAppendSubmit: () => void;
  onCreateExperiment?: () => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   创建入口必须是独立弹窗，不能占用看板主文档流。
 *
 * Code Logic（这个函数做什么）:
 *   用共享 Dialog 渲染 AI 区、任务/块表单或追加表单，以及显式 createAction 按钮。
 */
export function OrchestratorCreateDialog(props: OrchestratorCreateDialogProps): JSX.Element {
  const {
    open,
    dialogKind,
    createMode,
    preferredCreateAction,
    form,
    blockTitle,
    blockMembers,
    completionPrompt,
    completingPrompt,
    creatingAction,
    appending,
    canCreate,
    canCreateBlock,
    canAppend,
    canCompletePrompt,
    canCreateTaskBlock,
    completionPromptRef,
    creatingExperiment = false,
    onClose,
    onCreateModeChange,
    onCompletionPromptChange,
    onUpdateFormField,
    onBlockTitleChange,
    onUpdateBlockMember,
    onAddBlockMember,
    onRemoveBlockMember,
    onCompleteWithAi,
    onCreateFormSubmit,
    onCreateAction,
    onAppendSubmit,
    onCreateExperiment,
  } = props;
  const { t } = useTranslation(['orchestrator', 'common']);
  const busy = Boolean(creatingAction) || completingPrompt || creatingExperiment || appending;
  const isAppend = dialogKind === 'append';
  const isBlock = !isAppend && createMode === 'taskBlock';
  const title = isAppend
    ? t('orchestrator:create.appendTitle')
    : isBlock
      ? t('orchestrator:create.modeBlock')
      : t('orchestrator:create.title');
  const subtitle = isAppend
    ? t('orchestrator:create.appendSubtitle')
    : t('orchestrator:create.subtitle');

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
              {title}
            </h2>
            <p className={styles.sectionLead}>{subtitle}</p>
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
          {!isAppend ? (
            <div className={styles.createModeToggle} role="group" aria-label={t('orchestrator:create.title')}>
              <Button
                variant={createMode === 'task' ? 'primary' : 'secondary'}
                size="sm"
                disabled={busy}
                onClick={() => onCreateModeChange('task')}
              >
                {t('orchestrator:create.modeTask')}
              </Button>
              <Button
                variant={createMode === 'taskBlock' ? 'primary' : 'secondary'}
                size="sm"
                disabled={busy || !canCreateTaskBlock}
                onClick={() => onCreateModeChange('taskBlock')}
              >
                {t('orchestrator:create.modeBlock')}
              </Button>
            </div>
          ) : null}
          {!canCreateTaskBlock && isBlock ? (
            <p className={styles.sectionLead}>{t('orchestrator:create.unsupportedBlocks')}</p>
          ) : null}
          {!isAppend ? (
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
          ) : null}
          <form className={styles.form} onSubmit={onCreateFormSubmit}>
            {isBlock ? (
              <>
                <label className={styles.field}>
                  <span>{t('orchestrator:create.blockTitle')}</span>
                  <Input
                    value={blockTitle}
                    onChange={(event) => onBlockTitleChange(event.target.value)}
                    placeholder={t('orchestrator:create.blockTitlePlaceholder')}
                    aria-label={t('orchestrator:create.blockTitle')}
                    disabled={Boolean(creatingAction)}
                  />
                </label>
                {blockMembers.map((member, index) => (
                  <div className={styles.memberEditor} key={`block-member-${index}`}>
                    <div className={styles.memberEditorHeader}>
                      <span>{t('orchestrator:create.memberIndex', { index: index + 1 })}</span>
                      {blockMembers.length > MIN_ORCHESTRATOR_BLOCK_MEMBERS ? (
                        <Button
                          variant="ghost"
                          size="sm"
                          disabled={Boolean(creatingAction)}
                          onClick={() => onRemoveBlockMember(index)}
                        >
                          {t('orchestrator:create.removeMember')}
                        </Button>
                      ) : null}
                    </div>
                    <label className={styles.field}>
                      <span>{t('orchestrator:create.taskTitle')}</span>
                      <Input
                        value={member.title}
                        onChange={(event) => onUpdateBlockMember(index, 'title', event.target.value)}
                        placeholder={t('orchestrator:create.taskTitlePlaceholder')}
                        disabled={Boolean(creatingAction)}
                      />
                    </label>
                    <label className={styles.field}>
                      <span>{t('orchestrator:create.goal')}</span>
                      <textarea
                        className={styles.textarea}
                        value={member.goal}
                        onChange={(event) => onUpdateBlockMember(index, 'goal', event.target.value)}
                        placeholder={t('orchestrator:create.goalPlaceholder')}
                        rows={3}
                        disabled={Boolean(creatingAction)}
                      />
                    </label>
                    <label className={styles.field}>
                      <span>{t('orchestrator:create.acceptanceCriteria')}</span>
                      <textarea
                        className={styles.textarea}
                        value={member.acceptanceCriteria}
                        onChange={(event) =>
                          onUpdateBlockMember(index, 'acceptanceCriteria', event.target.value)
                        }
                        placeholder={t('orchestrator:create.acceptanceCriteriaPlaceholder')}
                        rows={3}
                        disabled={Boolean(creatingAction)}
                      />
                    </label>
                  </div>
                ))}
                {blockMembers.length < MAX_ORCHESTRATOR_BLOCK_MEMBERS ? (
                  <Button
                    variant="secondary"
                    size="sm"
                    icon={<PlusIcon />}
                    disabled={Boolean(creatingAction)}
                    onClick={onAddBlockMember}
                  >
                    {t('orchestrator:create.addMember')}
                  </Button>
                ) : null}
              </>
            ) : (
              <>
                <label className={styles.field}>
                  <span>{t('orchestrator:create.taskTitle')}</span>
                  <Input
                    value={form.title}
                    onChange={(event) => onUpdateFormField('title', event.target.value)}
                    placeholder={t('orchestrator:create.taskTitlePlaceholder')}
                    aria-label={t('orchestrator:create.taskTitle')}
                    disabled={Boolean(creatingAction) || appending}
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
                    disabled={Boolean(creatingAction) || appending}
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
                    disabled={Boolean(creatingAction) || appending}
                  />
                </label>
              </>
            )}
            <div className={styles.dialogActions}>
              <Button variant="ghost" size="md" disabled={busy} onClick={onClose}>
                {t('common:action.cancel')}
              </Button>
              {isAppend ? (
                <Button
                  variant="primary"
                  size="md"
                  type="button"
                  icon={<PlusIcon />}
                  loading={appending}
                  disabled={!canAppend || completingPrompt}
                  onClick={() => {
                    void onAppendSubmit();
                  }}
                >
                  {t('orchestrator:create.appendSubmit')}
                </Button>
              ) : (
                ORCHESTRATOR_CREATE_ACTIONS.map((action) => {
                  const preferred = preferredCreateAction === action.createAction;
                  const variant =
                    preferred || (!preferredCreateAction && action.createAction === 'start')
                      ? 'primary'
                      : 'secondary';
                  return (
                    <Button
                      variant={variant}
                      size="md"
                      type="button"
                      icon={<PlusIcon />}
                      key={action.createAction}
                      loading={creatingAction === action.createAction}
                      disabled={
                        (isBlock ? !canCreateBlock : !canCreate) ||
                        completingPrompt ||
                        creatingExperiment ||
                        (isBlock && !canCreateTaskBlock)
                      }
                      onClick={() => {
                        void onCreateAction(action.createAction);
                      }}
                    >
                      {t(action.labelKey)}
                    </Button>
                  );
                })
              )}
              {!isAppend && !isBlock && onCreateExperiment ? (
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
