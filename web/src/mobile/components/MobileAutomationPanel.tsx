import { useCallback, useEffect, useId, useRef, useState } from 'react';
import type { FormEvent, ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { httpOrchestratorTransport } from '@/api/workbenchHttp';
import { upsertOrchestratorTaskView } from '@/lib/orchestratorRemote';
import type {
  OrchestratorRemoteOutboxItem,
  OrchestratorRemoteOutboxStatus,
  OrchestratorTaskStatus,
  OrchestratorTaskView,
  WorkbenchProject,
} from '@/lib/types';
import styles from '../MobileWorkbench.module.css';

export interface MobileAutomationPanelProps {
  project: WorkbenchProject | null;
}

const MOBILE_AUTOMATION_STATUS_LABEL_KEYS: Record<
  OrchestratorTaskStatus,
  `workbench:mobile.automationPanel.status.${OrchestratorTaskStatus}`
> = {
  draft: 'workbench:mobile.automationPanel.status.draft',
  queued: 'workbench:mobile.automationPanel.status.queued',
  preparing: 'workbench:mobile.automationPanel.status.preparing',
  running: 'workbench:mobile.automationPanel.status.running',
  verifying: 'workbench:mobile.automationPanel.status.verifying',
  delivering: 'workbench:mobile.automationPanel.status.delivering',
  done: 'workbench:mobile.automationPanel.status.done',
  blocked: 'workbench:mobile.automationPanel.status.blocked',
  aborted: 'workbench:mobile.automationPanel.status.aborted',
};

const MOBILE_AUTOMATION_PENDING_STATUS_LABEL_KEYS: Record<
  OrchestratorRemoteOutboxStatus,
  `workbench:mobile.automationPanel.pendingStatus.${OrchestratorRemoteOutboxStatus}`
> = {
  pending: 'workbench:mobile.automationPanel.pendingStatus.pending',
  sending: 'workbench:mobile.automationPanel.pendingStatus.sending',
  mirrored: 'workbench:mobile.automationPanel.pendingStatus.mirrored',
  failed: 'workbench:mobile.automationPanel.pendingStatus.failed',
};

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 Orchestrator HTTP 请求失败时需要给用户展示后端返回的可读错误，而不是只显示 unknown。
 *
 * Code Logic（这个函数做什么）:
 *   读取 Error.message；如果抛出值不是 Error，则转成字符串作为兜底展示。
 */
function getErrorMessage(reason: unknown): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  return String(reason);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   远端离线创建落入 outbox 时仍应在手机端展示用户填写的任务标题，避免只看到设备路径。
 *
 * Code Logic（这个函数做什么）:
 *   从 outbox requestJson 中解析 title；解析失败时使用远端项目路径兜底。
 */
function pendingRemoteTaskTitle(item: OrchestratorRemoteOutboxItem): string {
  try {
    const value = JSON.parse(item.requestJson) as { title?: unknown };
    if (typeof value.title === 'string' && value.title.trim()) return value.title.trim();
  } catch {
    // requestJson 来自本机 outbox，异常时展示路径兜底即可。
  }
  return item.remoteProjectPath;
}

/**
 * MobileAutomationPanel（移动端项目级自动化面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端 Workbench 需要在已选择本机项目后直接创建项目级 Orchestrator 任务，并查看当前项目任务队列。
 *
 * Code Logic（这个组件做什么）:
 *   根据当前 project 读取 `/api/orchestrator/task-views/list`；创建任务通过独立弹窗完成，弹窗既支持手动填写
 *   title/goal/acceptanceCriteria，也支持调用 AI prompt completion 把简单 Prompt 填充为三字段后再提交。
 */
export function MobileAutomationPanel({ project }: MobileAutomationPanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const [taskViews, setTaskViews] = useState<OrchestratorTaskView[]>([]);
  const [title, setTitle] = useState<string>('');
  const [goal, setGoal] = useState<string>('');
  const [acceptanceCriteria, setAcceptanceCriteria] = useState<string>('');
  const [createDialogOpen, setCreateDialogOpen] = useState<boolean>(false);
  const [promptDraft, setPromptDraft] = useState<string>('');
  const [loading, setLoading] = useState<boolean>(false);
  const [creating, setCreating] = useState<boolean>(false);
  const [completingPrompt, setCompletingPrompt] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const requestIdRef = useRef<number>(0);
  const activeProjectIdRef = useRef<string | null>(null);
  const promptDraftRef = useRef<HTMLTextAreaElement | null>(null);
  const titleId = useId();
  const dialogTitleId = useId();
  const hasProject = Boolean(project);
  const trimmedPromptDraft = promptDraft.trim();
  const trimmedTitle = title.trim();
  const trimmedGoal = goal.trim();
  const trimmedAcceptanceCriteria = acceptanceCriteria.trim();
  const canCompletePrompt = Boolean(
    hasProject &&
      trimmedPromptDraft &&
      !completingPrompt &&
      !creating &&
      !loading,
  );
  const canSubmit = Boolean(
    hasProject &&
      trimmedTitle &&
      trimmedGoal &&
      trimmedAcceptanceCriteria &&
      !creating &&
      !completingPrompt &&
      !loading,
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   自动化任务列表需要手动刷新和项目切换时自动刷新，并防止旧项目响应覆盖当前项目。
   *
   * Code Logic（这个函数做什么）:
   *   按 projectId 调用 HTTP list route；用递增 request id 和 active project ref 做 stale guard。
   */
  const loadTasks = useCallback(async (projectId: string): Promise<void> => {
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;
    setLoading(true);
    setError(null);

    try {
      const nextTaskViews = await httpOrchestratorTransport.tasks.listViews(projectId);
      if (requestIdRef.current !== requestId) return;
      if (activeProjectIdRef.current !== projectId) return;
      setTaskViews(nextTaskViews);
    } catch (reason) {
      if (requestIdRef.current !== requestId) return;
      if (activeProjectIdRef.current !== projectId) return;
      setError(`${t('workbench:mobile.automationPanel.errors.list')}: ${getErrorMessage(reason)}`);
    } finally {
      if (requestIdRef.current === requestId && activeProjectIdRef.current === projectId) {
        setLoading(false);
      }
    }
  }, [t]);

  /* eslint-disable react-hooks/set-state-in-effect -- 项目切换时必须同步自动化任务上下文 */
  useEffect(() => {
    const projectId = project?.id ?? null;
    activeProjectIdRef.current = projectId;
    requestIdRef.current += 1;
    setTaskViews([]);
    setError(null);
    setStatus(null);
    setLoading(false);
    setCreating(false);
    setCompletingPrompt(false);
    setCreateDialogOpen(false);
    setPromptDraft('');
    setTitle('');
    setGoal('');
    setAcceptanceCriteria('');

    if (projectId) {
      void loadTasks(projectId);
    }
  }, [loadTasks, project?.id]);
  /* eslint-enable react-hooks/set-state-in-effect */

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击刷新时需要重新读取当前本机项目的任务列表，远端或未选项目不应发起请求。
   *
   * Code Logic（这个函数做什么）:
   *   校验当前 local project id，存在时调用 loadTasks；错误由 loadTasks 写入面板状态。
   */
  const handleRefresh = useCallback((): void => {
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    void loadTasks(projectId);
  }, [loadTasks]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端创建任务入口需要从列表页进入独立弹窗，避免表单常驻挤占任务队列空间。
   *
   * Code Logic（这个函数做什么）:
   *   打开创建弹窗并清理上一轮状态提示；表单草稿保留，让用户误关前可继续编辑。
   */
  const handleOpenCreateDialog = useCallback((): void => {
    if (!activeProjectIdRef.current) return;
    setError(null);
    setStatus(null);
    setCreateDialogOpen(true);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   弹窗关闭应避免打断正在创建或 AI 完善的请求，防止用户误以为操作已取消。
   *
   * Code Logic（这个函数做什么）:
   *   若没有 pending 请求则关闭 dialog；请求中忽略关闭动作。
   */
  const handleCloseCreateDialog = useCallback((): void => {
    if (creating || completingPrompt) return;
    setCreateDialogOpen(false);
  }, [creating, completingPrompt]);

  useEffect(() => {
    if (!createDialogOpen) return undefined;
    const focusTimer = window.setTimeout(() => {
      promptDraftRef.current?.focus();
    }, 0);
    return () => {
      window.clearTimeout(focusTimer);
    };
  }, [createDialogOpen]);

  useEffect(() => {
    if (!createDialogOpen) return undefined;
    const handleKeyDown = (event: KeyboardEvent): void => {
      if (event.key === 'Escape') {
        handleCloseCreateDialog();
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => {
      window.removeEventListener('keydown', handleKeyDown);
    };
  }, [createDialogOpen, handleCloseCreateDialog]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户常会只输入一句简单需求，手机端也应能像桌面端一样让 AI 结构化生成任务标题、目标和验收标准。
   *
   * Code Logic（这个函数做什么）:
   *   校验当前项目和 prompt 后调用 HTTP complete-prompt route；成功时把返回的三字段填入创建表单。
   */
  const handleCompletePrompt = useCallback(async (): Promise<void> => {
    const projectId = activeProjectIdRef.current;
    const workingDirectory = project?.kind === 'local' ? project.path : null;
    if (!projectId) {
      setError(t('workbench:mobile.automationPanel.noProject'));
      return;
    }
    if (!trimmedPromptDraft) {
      setError(t('workbench:mobile.automationPanel.errors.promptRequired'));
      return;
    }

    setCompletingPrompt(true);
    setError(null);
    setStatus(null);
    try {
      const completed = await httpOrchestratorTransport.tasks.completePrompt({
        projectId,
        prompt: trimmedPromptDraft,
        workingDirectory,
      });
      if (activeProjectIdRef.current !== projectId) return;
      setTitle(completed.title.trim());
      setGoal(completed.goal.trim());
      setAcceptanceCriteria(completed.acceptanceCriteria.trim());
    } catch (reason) {
      if (activeProjectIdRef.current !== projectId) return;
      setError(
        `${t('workbench:mobile.automationPanel.errors.completePrompt')}: ${getErrorMessage(reason)}`,
      );
    } finally {
      if (activeProjectIdRef.current === projectId) {
        setCompletingPrompt(false);
      }
    }
  }, [project, t, trimmedPromptDraft]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在手机端填写任务表单后，需要立即创建并入队当前项目 Orchestrator 任务。
   *
   * Code Logic（这个函数做什么）:
   *   阻止默认提交，校验本机项目和必填字段，调用 HTTP create route；成功后清空表单并把返回任务合并进列表。
   */
  const handleSubmit = useCallback(async (event: FormEvent<HTMLFormElement>): Promise<void> => {
    event.preventDefault();
    const projectId = activeProjectIdRef.current;
    if (!projectId) {
      setError(t('workbench:mobile.automationPanel.noProject'));
      return;
    }
    if (!trimmedTitle || !trimmedGoal || !trimmedAcceptanceCriteria) {
      setError(t('workbench:mobile.automationPanel.errors.required'));
      return;
    }

    setCreating(true);
    setError(null);
    setStatus(null);
    try {
      const createdTaskView = await httpOrchestratorTransport.tasks.createView({
        projectId,
        title: trimmedTitle,
        goal: trimmedGoal,
        acceptanceCriteria: trimmedAcceptanceCriteria,
        priority: 0,
      });
      if (activeProjectIdRef.current !== projectId) return;
      setTaskViews((current) => upsertOrchestratorTaskView(current, createdTaskView));
      setTitle('');
      setGoal('');
      setAcceptanceCriteria('');
      setPromptDraft('');
      setCreateDialogOpen(false);
      setStatus(t('workbench:mobile.automationPanel.created'));
    } catch (reason) {
      if (activeProjectIdRef.current !== projectId) return;
      setError(
        `${t('workbench:mobile.automationPanel.errors.create')}: ${getErrorMessage(reason)}`,
      );
    } finally {
      if (activeProjectIdRef.current === projectId) {
        setCreating(false);
      }
    }
  }, [t, trimmedAcceptanceCriteria, trimmedGoal, trimmedTitle]);

  return (
    <section className={styles.panel} aria-labelledby={titleId}>
      <div className={styles.panelHeaderRow}>
        <div className={styles.panelHeader}>
          <p className={styles.panelKicker}>{t('workbench:mobile.kicker')}</p>
          <h1 id={titleId}>{t('workbench:mobile.automationPanel.title')}</h1>
        </div>
        <div className={styles.panelHeaderActions}>
          <button
            type="button"
            className={styles.secondaryButton}
            disabled={!hasProject || loading}
            onClick={handleRefresh}
          >
            {t('workbench:refresh')}
          </button>
          <button
            type="button"
            className={styles.mobileTerminalPrimaryButton}
            disabled={!hasProject || loading}
            onClick={handleOpenCreateDialog}
          >
            {t('workbench:mobile.automationPanel.createOpen')}
          </button>
        </div>
      </div>

      {!project ? (
        <p className={styles.panelState}>{t('workbench:mobile.automationPanel.noProject')}</p>
      ) : null}
      {error ? (
        <p className={styles.panelError}>
          <span>{t('workbench:mobile.projectPanel.error')}</span>
          <span>{error}</span>
        </p>
      ) : null}
      {status ? <p className={styles.panelState}>{status}</p> : null}

      {hasProject ? (
        <>
          {loading ? <p className={styles.panelState}>{t('workbench:loading')}</p> : null}
          {!loading && taskViews.length === 0 ? (
            <p className={styles.panelState}>{t('workbench:mobile.automationPanel.empty')}</p>
          ) : null}

          <div
            className={styles.mobileList}
            aria-label={t('workbench:mobile.automationPanel.listAriaLabel')}
          >
            {taskViews.map((view) => {
              if (view.origin === 'pendingRemote') {
                const item = view.item;
                return (
                  <article key={item.id} className={styles.mobileListItem}>
                    <div className={styles.mobileListTitleRow}>
                      <strong className={styles.mobileListTitle}>
                        {pendingRemoteTaskTitle(item)}
                      </strong>
                      <span className={`${styles.mobileBadge} ${styles.mobileBadgeAccent}`}>
                        {t(MOBILE_AUTOMATION_PENDING_STATUS_LABEL_KEYS[item.status])}
                      </span>
                    </div>
                    <div className={styles.automationTaskBody}>
                      <p>{item.deviceName}</p>
                      <p>{item.lastError ?? item.remoteProjectPath}</p>
                    </div>
                  </article>
                );
              }

              const task = view.task;
              return (
                <article key={task.id} className={styles.mobileListItem}>
                  <div className={styles.mobileListTitleRow}>
                    <strong className={styles.mobileListTitle}>{task.title}</strong>
                    <span className={`${styles.mobileBadge} ${styles.mobileBadgeAccent}`}>
                      {t(MOBILE_AUTOMATION_STATUS_LABEL_KEYS[task.status])}
                    </span>
                  </div>
                  <div className={styles.automationTaskBody}>
                    <p>{task.goal}</p>
                    <p>{task.acceptanceCriteria}</p>
                  </div>
                  <div className={styles.mobileBadgeRow}>
                    <span className={styles.mobileBadge}>
                      {t('workbench:mobile.automationPanel.priority', {
                        priority: task.priority,
                      })}
                    </span>
                    <span className={styles.mobileListMeta}>
                      {t('workbench:mobile.automationPanel.attempt', { attempt: task.attempt })}
                    </span>
                  </div>
                </article>
              );
            })}
          </div>

          {createDialogOpen ? (
            <div
              className={styles.mobileDialogOverlay}
              onMouseDown={(event) => {
                if (event.target === event.currentTarget) {
                  handleCloseCreateDialog();
                }
              }}
            >
              <div
                className={styles.mobileDialog}
                role="dialog"
                aria-modal="true"
                aria-labelledby={dialogTitleId}
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
                    onClick={handleCloseCreateDialog}
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
                          setPromptDraft(event.target.value);
                          setStatus(null);
                        }}
                      />
                    </label>
                    <button
                      type="button"
                      className={styles.mobileTerminalPrimaryButton}
                      disabled={!canCompletePrompt}
                      onClick={() => {
                        void handleCompletePrompt();
                      }}
                    >
                      {completingPrompt
                        ? t('workbench:mobile.automationPanel.completingPrompt')
                        : t('workbench:mobile.automationPanel.completeWithAi')}
                    </button>
                  </div>

                  <form className={styles.mobileFormInline} onSubmit={handleSubmit}>
                    <label className={styles.mobileField}>
                      <span>{t('workbench:mobile.automationPanel.fields.title')}</span>
                      <input
                        className={styles.mobileInput}
                        value={title}
                        disabled={creating || completingPrompt}
                        placeholder={t('workbench:mobile.automationPanel.placeholders.title')}
                        onChange={(event) => {
                          setTitle(event.target.value);
                          setStatus(null);
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
                          setGoal(event.target.value);
                          setStatus(null);
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
                          setAcceptanceCriteria(event.target.value);
                          setStatus(null);
                        }}
                      />
                    </label>

                    <button
                      type="submit"
                      className={styles.mobileTerminalPrimaryButton}
                      disabled={!canSubmit}
                    >
                      {creating
                        ? t('workbench:mobile.automationPanel.creating')
                        : t('workbench:mobile.automationPanel.create')}
                    </button>
                  </form>
                </div>
              </div>
            </div>
          ) : null}
        </>
      ) : null}
    </section>
  );
}
