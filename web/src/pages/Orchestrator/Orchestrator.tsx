/**
 * Orchestrator 页面 - 自动化任务编排入口
 *
 * Business Logic（为什么需要这个页面）:
 *   用户需要在当前 Workbench 项目下管理项目级自动化任务队列，创建任务、查看任务详情，并把 draft 任务手动入队。
 *   页面同时只读展示当前项目策略，帮助用户确认并发、验证命令以及提交/推送/合并等执行边界。
 *   当前前端只提供任务与项目策略入口，不启动 scheduler/runner/delivery，也不打开 Workbench deep link。
 *
 * Code Logic（这个组件做什么）:
 *   - 按 activeProject 拉取 Orchestrator 任务列表并按状态分组展示
 *   - 按 activeProject 拉取项目策略并在右侧卡片展示
 *   - 提供 title/goal/acceptanceCriteria 三个单行输入创建任务
 *   - 允许选中的 draft 任务切换为 queued，并只更新当前列表中的同一任务
 *   - 创建成功后把新任务插入列表顶部、选中新任务并清空表单
 *   - 不启动 scheduler/runner/delivery，也不处理 Workbench deep link 跳转
 *   - hooks 全部位于渲染分支之前，避免 early return 破坏调用顺序
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { FormEvent, JSX } from 'react';
import { useTranslation } from 'react-i18next';
import { orchestratorApi } from '@/api/orchestrator';
import { Button, Card, Input, Pill } from '@/components/primitives';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
import { PlayIcon, PlusIcon } from '@/lib/icons';
import {
  canQueueOrchestratorTask,
  groupOrchestratorTasks,
  ORCHESTRATOR_STATUSES,
  orchestratorCreateResultMatchesProject,
  orchestratorStatusTone,
  resolveOrchestratorTaskLoad,
} from '@/lib/orchestrator';
import type {
  OrchestratorProjectConfig,
  OrchestratorTask,
  OrchestratorTaskStatus,
} from '@/lib/types';
import styles from './Orchestrator.module.css';

/**
 * Business Logic（为什么需要这个类型）:
 *   i18next v26 对动态 key 有严格类型校验，状态文案需要提前收敛为静态 key 联合。
 *
 * Code Logic（这个类型做什么）:
 *   枚举 Orchestrator 所有状态对应的完整 i18n key。
 */
type OrchestratorStatusLabelKey =
  | 'orchestrator:status.draft'
  | 'orchestrator:status.queued'
  | 'orchestrator:status.preparing'
  | 'orchestrator:status.running'
  | 'orchestrator:status.verifying'
  | 'orchestrator:status.delivering'
  | 'orchestrator:status.done'
  | 'orchestrator:status.blocked'
  | 'orchestrator:status.aborted';

/**
 * Business Logic（为什么需要这个类型）:
 *   创建任务表单需要同时管理标题、目标和验收标准，集中成对象便于清空和提交校验。
 *
 * Code Logic（这个类型做什么）:
 *   定义页面本地表单状态，字段与 createTask 请求文本字段一一对应。
 */
interface OrchestratorCreateForm {
  title: string;
  goal: string;
  acceptanceCriteria: string;
}

const EMPTY_FORM: OrchestratorCreateForm = {
  title: '',
  goal: '',
  acceptanceCriteria: '',
};

const STATUS_LABEL_KEYS: Record<OrchestratorTaskStatus, OrchestratorStatusLabelKey> = {
  draft: 'orchestrator:status.draft',
  queued: 'orchestrator:status.queued',
  preparing: 'orchestrator:status.preparing',
  running: 'orchestrator:status.running',
  verifying: 'orchestrator:status.verifying',
  delivering: 'orchestrator:status.delivering',
  done: 'orchestrator:status.done',
  blocked: 'orchestrator:status.blocked',
  aborted: 'orchestrator:status.aborted',
};

/**
 * Business Logic（为什么需要这个函数）:
 *   API 调用失败时页面需要优先显示后端返回的可读错误，并在缺少 message 时回退到本地化通用提示。
 *
 * Code Logic（这个函数做什么）:
 *   从 unknown 错误中提取非空字符串；如果无法提取，返回调用方传入的 i18n fallback。
 */
function displayOrchestratorErrorMessage(error: unknown, fallback: string): string {
  const message =
    error instanceof Error ? error.message : typeof error === 'string' ? error : '';
  return message.trim() || fallback;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   任务详情需要展示创建/更新时间，让用户判断队列信息是否新鲜。
 *
 * Code Logic（这个函数做什么）:
 *   将 ISO 时间字符串转换为浏览器本地短日期时间；解析失败时保留原始字符串。
 */
function formatTaskTimestamp(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString([], {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/**
 * Orchestrator 页面组件
 *
 * @returns Orchestrator 路由的自动化任务 shell
 */
export function Orchestrator(): JSX.Element {
  const { t } = useTranslation(['orchestrator', 'nav', 'common']);
  const { activeProject, projectsLoading } = useWorkbenchProjects();
  const [tasks, setTasks] = useState<OrchestratorTask[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [form, setForm] = useState<OrchestratorCreateForm>(EMPTY_FORM);
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [queueingTaskId, setQueueingTaskId] = useState<string | null>(null);
  const [projectConfig, setProjectConfig] = useState<OrchestratorProjectConfig | null>(null);
  const [projectConfigLoading, setProjectConfigLoading] = useState(true);
  const [projectConfigError, setProjectConfigError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const activeProjectId = activeProject?.id ?? null;
  const activeProjectIdRef = useRef<string | null>(activeProjectId);
  activeProjectIdRef.current = activeProjectId;

  const groups = useMemo(() => groupOrchestratorTasks(tasks), [tasks]);
  const selectedTask = useMemo(() => {
    return tasks.find((task) => task.id === selectedTaskId) ?? tasks[0] ?? null;
  }, [selectedTaskId, tasks]);
  const taskLoadDecision = useMemo(
    () => resolveOrchestratorTaskLoad(projectsLoading, activeProjectId),
    [activeProjectId, projectsLoading],
  );
  const selectedTaskCanQueue = canQueueOrchestratorTask(selectedTask);

  const canCreate =
    Boolean(activeProjectId) &&
    form.title.trim().length > 0 &&
    form.goal.trim().length > 0 &&
    form.acceptanceCriteria.trim().length > 0 &&
    !creating;

  useEffect(() => {
    if (taskLoadDecision.kind === 'waiting') {
      setLoading(true);
      setError(null);
      return undefined;
    }

    if (taskLoadDecision.kind === 'empty') {
      setTasks([]);
      setSelectedTaskId(null);
      setLoading(false);
      setError(null);
      return undefined;
    }

    let cancelled = false;
    setLoading(true);
    setError(null);
    void orchestratorApi
      .listTasks(taskLoadDecision.projectId)
      .then((nextTasks) => {
        if (cancelled) return;
        setTasks(nextTasks);
        setSelectedTaskId((current) => {
          if (current && nextTasks.some((task) => task.id === current)) return current;
          return nextTasks[0]?.id ?? null;
        });
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setError(displayOrchestratorErrorMessage(err, t('orchestrator:errors.load')));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [taskLoadDecision, t]);

  useEffect(() => {
    if (taskLoadDecision.kind === 'waiting') {
      setProjectConfig(null);
      setProjectConfigLoading(true);
      setProjectConfigError(null);
      return undefined;
    }

    if (taskLoadDecision.kind === 'empty') {
      setProjectConfig(null);
      setProjectConfigLoading(false);
      setProjectConfigError(null);
      return undefined;
    }

    let cancelled = false;
    const projectId = taskLoadDecision.projectId;
    setProjectConfig(null);
    setProjectConfigLoading(true);
    setProjectConfigError(null);
    void orchestratorApi
      .getProjectConfig(projectId)
      .then((config) => {
        if (cancelled || activeProjectIdRef.current !== projectId) return;
        setProjectConfig(config);
      })
      .catch((err: unknown) => {
        if (cancelled || activeProjectIdRef.current !== projectId) return;
        setProjectConfigError(
          displayOrchestratorErrorMessage(err, t('orchestrator:errors.policy')),
        );
      })
      .finally(() => {
        if (!cancelled && activeProjectIdRef.current === projectId) {
          setProjectConfigLoading(false);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [taskLoadDecision, t]);

  const updateFormField = useCallback(
    (field: keyof OrchestratorCreateForm, value: string) => {
      setForm((current) => ({ ...current, [field]: value }));
    },
    [],
  );

  const handleCreateTask = useCallback(
    async (event: FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      if (!activeProject) {
        setError(t('orchestrator:errors.noProject'));
        return;
      }
      const projectId = activeProject.id;
      const payload = {
        projectId,
        title: form.title.trim(),
        goal: form.goal.trim(),
        acceptanceCriteria: form.acceptanceCriteria.trim(),
      };
      if (!payload.title || !payload.goal || !payload.acceptanceCriteria) {
        setError(t('orchestrator:errors.required'));
        return;
      }
      setCreating(true);
      setError(null);
      try {
        const created = await orchestratorApi.createTask(payload);
        if (!orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId)) {
          return;
        }
        setTasks((current) => [created, ...current.filter((task) => task.id !== created.id)]);
        setSelectedTaskId(created.id);
        setForm(EMPTY_FORM);
      } catch (err) {
        setError(displayOrchestratorErrorMessage(err, t('orchestrator:errors.create')));
      } finally {
        setCreating(false);
      }
    },
    [activeProject, form, t],
  );

  const handleQueueSelectedTask = useCallback(async () => {
    if (!selectedTask || !canQueueOrchestratorTask(selectedTask)) return;
    const taskId = selectedTask.id;
    const projectId = selectedTask.projectId;
    setQueueingTaskId(taskId);
    setError(null);
    try {
      const queued = await orchestratorApi.queueTask(taskId);
      if (!orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId)) {
        return;
      }
      setTasks((current) => current.map((task) => (task.id === queued.id ? queued : task)));
      setSelectedTaskId(queued.id);
    } catch (err) {
      if (orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId)) {
        setError(displayOrchestratorErrorMessage(err, t('orchestrator:errors.queue')));
      }
    } finally {
      setQueueingTaskId(null);
    }
  }, [selectedTask, t]);

  return (
    <div className={styles.page}>
      <header className={styles.header}>
        <div className={styles.headerText}>
          <span className={styles.eyebrow}>{t('nav:orchestrator')}</span>
          <h1 className={styles.title}>{t('orchestrator:title')}</h1>
          <p className={styles.subtitle}>{t('orchestrator:subtitle')}</p>
        </div>
        <div className={styles.projectStatus}>
          <Pill tone={activeProject ? 'success' : 'warn'} dot>
            {activeProject ? activeProject.name : t('orchestrator:noProject')}
          </Pill>
        </div>
      </header>

      {error ? (
        <div className={styles.error} role="alert">
          {error}
        </div>
      ) : null}

      <div className={styles.grid}>
        <Card variant="outlined" padding="md" className={styles.queue}>
          <Card.Header className={styles.cardHeader}>
            <div>
              <h2 className={styles.sectionTitle}>{t('orchestrator:queue.title')}</h2>
              <p className={styles.sectionLead}>{t('orchestrator:queue.subtitle')}</p>
            </div>
            <Pill tone="neutral">{tasks.length}</Pill>
          </Card.Header>
          <Card.Body className={styles.queueBody}>
            {loading ? <p className={styles.muted}>{t('common:loading')}</p> : null}
            {!loading && tasks.length === 0 ? (
              <div className={styles.empty}>
                <h3 className={styles.emptyTitle}>{t('orchestrator:emptyTitle')}</h3>
                <p className={styles.emptyBody}>{t('orchestrator:emptyBody')}</p>
              </div>
            ) : null}
            {!loading && tasks.length > 0
              ? ORCHESTRATOR_STATUSES.map((status) => (
                  <section className={styles.group} key={status}>
                    <div className={styles.groupHeader}>
                      <span>{t(STATUS_LABEL_KEYS[status])}</span>
                      <Pill tone={orchestratorStatusTone(status)}>{groups[status].length}</Pill>
                    </div>
                    <div className={styles.taskList}>
                      {groups[status].map((task) => {
                        const active = selectedTask?.id === task.id;
                        return (
                          <button
                            className={`${styles.task} ${active ? styles.taskActive : ''}`}
                            type="button"
                            aria-pressed={active}
                            aria-label={t('orchestrator:queue.taskAria', { title: task.title })}
                            key={task.id}
                            onClick={() => setSelectedTaskId(task.id)}
                          >
                            <span className={styles.taskTitle}>{task.title}</span>
                            <span className={styles.taskMeta}>
                              {t('orchestrator:queue.priority', { priority: task.priority })}
                            </span>
                          </button>
                        );
                      })}
                    </div>
                  </section>
                ))
              : null}
          </Card.Body>
        </Card>

        <div className={styles.detail}>
          <Card variant="outlined" padding="md">
            <Card.Header className={styles.cardHeader}>
              <div>
                <h2 className={styles.sectionTitle}>{t('orchestrator:detail.title')}</h2>
                <p className={styles.sectionLead}>{t('orchestrator:detail.subtitle')}</p>
              </div>
              <div className={styles.detailActions}>
                {selectedTaskCanQueue ? (
                  <Button
                    variant="primary"
                    size="sm"
                    icon={<PlayIcon />}
                    loading={queueingTaskId === selectedTask?.id}
                    onClick={handleQueueSelectedTask}
                  >
                    {t('orchestrator:detail.queue')}
                  </Button>
                ) : null}
                {selectedTask ? (
                  <Pill tone={orchestratorStatusTone(selectedTask.status)} dot>
                    {t(STATUS_LABEL_KEYS[selectedTask.status])}
                  </Pill>
                ) : null}
              </div>
            </Card.Header>
            <Card.Body className={styles.detailBody}>
              {selectedTask ? (
                <>
                  <div className={styles.detailTitleRow}>
                    <h3 className={styles.detailTitle}>{selectedTask.title}</h3>
                  </div>
                  <div className={styles.detailBlock}>
                    <span className={styles.label}>{t('orchestrator:detail.goal')}</span>
                    <p className={styles.detailText}>{selectedTask.goal}</p>
                  </div>
                  <div className={styles.detailBlock}>
                    <span className={styles.label}>
                      {t('orchestrator:detail.acceptanceCriteria')}
                    </span>
                    <p className={styles.detailText}>{selectedTask.acceptanceCriteria}</p>
                  </div>
                  <dl className={styles.metaGrid}>
                    <div>
                      <dt>{t('orchestrator:detail.branch')}</dt>
                      <dd>{selectedTask.branchName ?? t('orchestrator:detail.unassigned')}</dd>
                    </div>
                    <div>
                      <dt>{t('orchestrator:detail.attempt')}</dt>
                      <dd>{selectedTask.attempt}</dd>
                    </div>
                    <div>
                      <dt>{t('orchestrator:detail.createdAt')}</dt>
                      <dd>{formatTaskTimestamp(selectedTask.createdAt)}</dd>
                    </div>
                    <div>
                      <dt>{t('orchestrator:detail.updatedAt')}</dt>
                      <dd>{formatTaskTimestamp(selectedTask.updatedAt)}</dd>
                    </div>
                  </dl>
                  {selectedTask.blockedReason ? (
                    <div className={styles.blockedReason}>
                      <span className={styles.label}>{t('orchestrator:detail.blockedReason')}</span>
                      <p>{selectedTask.blockedReason}</p>
                    </div>
                  ) : null}
                </>
              ) : (
                <div className={styles.empty}>
                  <h3 className={styles.emptyTitle}>{t('orchestrator:emptyTitle')}</h3>
                  <p className={styles.emptyBody}>{t('orchestrator:emptyBody')}</p>
                </div>
              )}
            </Card.Body>
          </Card>

          <Card variant="outlined" padding="md" className={styles.createCard}>
            <Card.Header className={styles.cardHeader}>
              <div>
                <h2 className={styles.sectionTitle}>{t('orchestrator:create.title')}</h2>
                <p className={styles.sectionLead}>{t('orchestrator:create.subtitle')}</p>
              </div>
            </Card.Header>
            <Card.Body>
              <form className={styles.form} onSubmit={handleCreateTask}>
                <label className={styles.field}>
                  <span>{t('orchestrator:create.taskTitle')}</span>
                  <Input
                    value={form.title}
                    onChange={(event) => updateFormField('title', event.target.value)}
                    placeholder={t('orchestrator:create.taskTitlePlaceholder')}
                    aria-label={t('orchestrator:create.taskTitle')}
                  />
                </label>
                <label className={styles.field}>
                  <span>{t('orchestrator:create.goal')}</span>
                  <Input
                    value={form.goal}
                    onChange={(event) => updateFormField('goal', event.target.value)}
                    placeholder={t('orchestrator:create.goalPlaceholder')}
                    aria-label={t('orchestrator:create.goal')}
                  />
                </label>
                <label className={styles.field}>
                  <span>{t('orchestrator:create.acceptanceCriteria')}</span>
                  <Input
                    value={form.acceptanceCriteria}
                    onChange={(event) =>
                      updateFormField('acceptanceCriteria', event.target.value)
                    }
                    placeholder={t('orchestrator:create.acceptanceCriteriaPlaceholder')}
                    aria-label={t('orchestrator:create.acceptanceCriteria')}
                  />
                </label>
                <Button
                  variant="primary"
                  size="md"
                  type="submit"
                  icon={<PlusIcon />}
                  loading={creating}
                  disabled={!canCreate}
                >
                  {t('orchestrator:create.submit')}
                </Button>
              </form>
            </Card.Body>
          </Card>
        </div>

        <Card variant="outlined" padding="md" className={styles.policy}>
          <Card.Header className={styles.cardHeader}>
            <div>
              <h2 className={styles.sectionTitle}>{t('orchestrator:policy.title')}</h2>
              <p className={styles.sectionLead}>{t('orchestrator:policy.subtitle')}</p>
            </div>
            {projectConfig ? (
              <Pill tone={projectConfig.enabled ? 'success' : 'warn'} dot>
                {projectConfig.enabled
                  ? t('orchestrator:policy.enabled')
                  : t('orchestrator:policy.disabled')}
              </Pill>
            ) : null}
          </Card.Header>
          <Card.Body className={styles.policyBody}>
            {projectConfigLoading ? <p className={styles.muted}>{t('common:loading')}</p> : null}
            {!projectConfigLoading && projectConfigError ? (
              <div className={styles.policyError} role="alert">
                {projectConfigError}
              </div>
            ) : null}
            {!projectConfigLoading && !projectConfig && !projectConfigError ? (
              <div className={styles.empty}>
                <h3 className={styles.emptyTitle}>{t('orchestrator:policy.emptyTitle')}</h3>
                <p className={styles.emptyBody}>{t('orchestrator:policy.emptyBody')}</p>
              </div>
            ) : null}
            {projectConfig ? (
              <>
                <dl className={styles.policyGrid}>
                  <div>
                    <dt>{t('orchestrator:policy.maxConcurrentTasks')}</dt>
                    <dd>{projectConfig.maxConcurrentTasks}</dd>
                  </div>
                  <div>
                    <dt>{t('orchestrator:policy.branchPrefix')}</dt>
                    <dd>{projectConfig.branchPrefix}</dd>
                  </div>
                  <div>
                    <dt>{t('orchestrator:policy.retryLimit')}</dt>
                    <dd>{projectConfig.retryLimit}</dd>
                  </div>
                  <div>
                    <dt>{t('orchestrator:policy.autoCommit')}</dt>
                    <dd>
                      {projectConfig.autoCommit
                        ? t('orchestrator:policy.on')
                        : t('orchestrator:policy.off')}
                    </dd>
                  </div>
                  <div>
                    <dt>{t('orchestrator:policy.autoPushTaskBranch')}</dt>
                    <dd>
                      {projectConfig.autoPushTaskBranch
                        ? t('orchestrator:policy.on')
                        : t('orchestrator:policy.off')}
                    </dd>
                  </div>
                  <div>
                    <dt>{t('orchestrator:policy.autoMergeToMain')}</dt>
                    <dd>
                      {projectConfig.autoMergeToMain
                        ? t('orchestrator:policy.on')
                        : t('orchestrator:policy.off')}
                    </dd>
                  </div>
                  <div>
                    <dt>{t('orchestrator:policy.autoPushMain')}</dt>
                    <dd>
                      {projectConfig.autoPushMain
                        ? t('orchestrator:policy.on')
                        : t('orchestrator:policy.off')}
                    </dd>
                  </div>
                  <div>
                    <dt>{t('orchestrator:policy.retainWorktreeOnDone')}</dt>
                    <dd>
                      {projectConfig.retainWorktreeOnDone
                        ? t('orchestrator:policy.on')
                        : t('orchestrator:policy.off')}
                    </dd>
                  </div>
                  <div>
                    <dt>{t('orchestrator:policy.retainWorktreeOnBlocked')}</dt>
                    <dd>
                      {projectConfig.retainWorktreeOnBlocked
                        ? t('orchestrator:policy.on')
                        : t('orchestrator:policy.off')}
                    </dd>
                  </div>
                </dl>
                <div className={styles.policyCommands}>
                  <span className={styles.label}>
                    {t('orchestrator:policy.verificationCommands')}
                  </span>
                  {projectConfig.verificationCommands.length > 0 ? (
                    <ul className={styles.commandList}>
                      {projectConfig.verificationCommands.map((command, index) => (
                        <li className={styles.commandItem} key={`${command}-${index}`}>
                          {command}
                        </li>
                      ))}
                    </ul>
                  ) : (
                    <p className={styles.emptyBody}>
                      {t('orchestrator:policy.noVerificationCommands')}
                    </p>
                  )}
                </div>
              </>
            ) : null}
          </Card.Body>
        </Card>
      </div>
    </div>
  );
}
