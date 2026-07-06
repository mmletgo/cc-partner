import { useCallback, useEffect, useId, useRef, useState } from 'react';
import type { FormEvent, ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { httpOrchestratorTransport } from '@/api/workbenchHttp';
import type { OrchestratorTask, OrchestratorTaskStatus, WorkbenchProject } from '@/lib/types';
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
 * MobileAutomationPanel（移动端项目级自动化面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端 Workbench 需要在已选择本机项目后直接创建项目级 Orchestrator 任务，并查看当前项目任务队列。
 *
 * Code Logic（这个组件做什么）:
 *   根据当前 project 读取 `/api/orchestrator/tasks/list`，提交 title/goal/acceptanceCriteria 到 create route；
 *   create 请求默认由 HTTP transport 携带 queue=true 和 clientRequestId，成功后清空表单并更新任务列表。
 */
export function MobileAutomationPanel({ project }: MobileAutomationPanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const [tasks, setTasks] = useState<OrchestratorTask[]>([]);
  const [title, setTitle] = useState<string>('');
  const [goal, setGoal] = useState<string>('');
  const [acceptanceCriteria, setAcceptanceCriteria] = useState<string>('');
  const [loading, setLoading] = useState<boolean>(false);
  const [creating, setCreating] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const requestIdRef = useRef<number>(0);
  const activeLocalProjectIdRef = useRef<string | null>(null);
  const titleId = useId();
  const formId = useId();
  const isLocalProject = project?.kind === 'local';
  const trimmedTitle = title.trim();
  const trimmedGoal = goal.trim();
  const trimmedAcceptanceCriteria = acceptanceCriteria.trim();
  const canSubmit = Boolean(
    isLocalProject &&
      trimmedTitle &&
      trimmedGoal &&
      trimmedAcceptanceCriteria &&
      !creating &&
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
      const nextTasks = await httpOrchestratorTransport.tasks.list(projectId);
      if (requestIdRef.current !== requestId) return;
      if (activeLocalProjectIdRef.current !== projectId) return;
      setTasks(nextTasks);
    } catch (reason) {
      if (requestIdRef.current !== requestId) return;
      if (activeLocalProjectIdRef.current !== projectId) return;
      setError(`${t('workbench:mobile.automationPanel.errors.list')}: ${getErrorMessage(reason)}`);
    } finally {
      if (requestIdRef.current === requestId && activeLocalProjectIdRef.current === projectId) {
        setLoading(false);
      }
    }
  }, [t]);

  /* eslint-disable react-hooks/set-state-in-effect -- 项目切换时必须同步自动化任务上下文 */
  useEffect(() => {
    const projectId = project?.kind === 'local' ? project.id : null;
    activeLocalProjectIdRef.current = projectId;
    requestIdRef.current += 1;
    setTasks([]);
    setError(null);
    setStatus(null);
    setLoading(false);
    setCreating(false);
    setTitle('');
    setGoal('');
    setAcceptanceCriteria('');

    if (projectId) {
      void loadTasks(projectId);
    }
  }, [loadTasks, project?.id, project?.kind]);
  /* eslint-enable react-hooks/set-state-in-effect */

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击刷新时需要重新读取当前本机项目的任务列表，远端或未选项目不应发起请求。
   *
   * Code Logic（这个函数做什么）:
   *   校验当前 local project id，存在时调用 loadTasks；错误由 loadTasks 写入面板状态。
   */
  const handleRefresh = useCallback((): void => {
    const projectId = activeLocalProjectIdRef.current;
    if (!projectId) return;
    void loadTasks(projectId);
  }, [loadTasks]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在手机端填写任务表单后，需要立即创建并入队当前项目 Orchestrator 任务。
   *
   * Code Logic（这个函数做什么）:
   *   阻止默认提交，校验本机项目和必填字段，调用 HTTP create route；成功后清空表单并把返回任务合并进列表。
   */
  const handleSubmit = useCallback(async (event: FormEvent<HTMLFormElement>): Promise<void> => {
    event.preventDefault();
    const projectId = activeLocalProjectIdRef.current;
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
      const createdTask = await httpOrchestratorTransport.tasks.create({
        projectId,
        title: trimmedTitle,
        goal: trimmedGoal,
        acceptanceCriteria: trimmedAcceptanceCriteria,
        priority: 0,
      });
      if (activeLocalProjectIdRef.current !== projectId) return;
      setTasks((current) => [
        createdTask,
        ...current.filter((task) => task.id !== createdTask.id),
      ]);
      setTitle('');
      setGoal('');
      setAcceptanceCriteria('');
      setStatus(t('workbench:mobile.automationPanel.created'));
    } catch (reason) {
      if (activeLocalProjectIdRef.current !== projectId) return;
      setError(
        `${t('workbench:mobile.automationPanel.errors.create')}: ${getErrorMessage(reason)}`,
      );
    } finally {
      if (activeLocalProjectIdRef.current === projectId) {
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
        <button
          type="button"
          className={styles.secondaryButton}
          disabled={!isLocalProject || loading}
          onClick={handleRefresh}
        >
          {t('workbench:refresh')}
        </button>
      </div>

      {!project ? (
        <p className={styles.panelState}>{t('workbench:mobile.automationPanel.noProject')}</p>
      ) : null}
      {project && !isLocalProject ? (
        <p className={styles.panelState}>
          {t('workbench:mobile.automationPanel.remoteUnsupported')}
        </p>
      ) : null}
      {error ? (
        <p className={styles.panelError}>
          <span>{t('workbench:mobile.projectPanel.error')}</span>
          <span>{error}</span>
        </p>
      ) : null}
      {status ? <p className={styles.panelState}>{status}</p> : null}

      {isLocalProject ? (
        <>
          <form id={formId} className={styles.mobileFormInline} onSubmit={handleSubmit}>
            <label className={styles.mobileField}>
              <span>{t('workbench:mobile.automationPanel.fields.title')}</span>
              <input
                className={styles.mobileInput}
                value={title}
                disabled={creating}
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
                disabled={creating}
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
                disabled={creating}
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

          {loading ? <p className={styles.panelState}>{t('workbench:loading')}</p> : null}
          {!loading && tasks.length === 0 ? (
            <p className={styles.panelState}>{t('workbench:mobile.automationPanel.empty')}</p>
          ) : null}

          <div
            className={styles.mobileList}
            aria-label={t('workbench:mobile.automationPanel.listAriaLabel')}
          >
            {tasks.map((task) => (
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
            ))}
          </div>
        </>
      ) : null}
    </section>
  );
}
