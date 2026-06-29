import { useCallback, useEffect, useRef, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import { MobileProjectPanel } from './components/MobileProjectPanel';
import { MobileWorkbenchShell } from './components/MobileWorkbenchShell';
import { MobileWorktreePanel } from './components/MobileWorktreePanel';
import {
  canSelectMobileProject,
  selectPreferredMobileSession,
  selectPreferredMobileWorktree,
  type MobileWorkbenchPanel,
} from './mobileWorkbenchState';
import styles from './MobileWorkbench.module.css';

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 HTTP 请求失败时需要展示可读错误，并兼容非 Error 抛出值。
 *
 * Code Logic（这个函数做什么）:
 *   把 unknown reason 规整为字符串；优先使用 Error.message，空值回退 String(reason)。
 */
function getErrorMessage(reason: unknown): string {
  if (reason instanceof Error && reason.message.trim()) {
    return reason.message;
  }
  return String(reason);
}

/**
 * MobileWorkbench（移动端工作台页面）
 *
 * Business Logic（为什么需要这个组件）:
 *   `/mobile` 需要通过 HTTP 加载最近项目，并在用户选择项目后加载对应 worktree 与 terminal session 上下文。
 *
 * Code Logic（这个组件做什么）:
 *   管理 active panel/project/worktree/session 状态，调用 httpWorkbenchTransport 拉取数据，用 request id 避免 stale 请求覆盖新选择。
 */
export function MobileWorkbench(): ReactElement {
  const [panel, setPanel] = useState<MobileWorkbenchPanel>('projects');
  const [projects, setProjects] = useState<WorkbenchProject[]>([]);
  const [activeProject, setActiveProject] = useState<WorkbenchProject | null>(null);
  const [worktrees, setWorktrees] = useState<WorkbenchWorktree[]>([]);
  const [activeWorktree, setActiveWorktree] = useState<WorkbenchWorktree | null>(null);
  const [sessions, setSessions] = useState<WorkbenchSession[]>([]);
  const [activeSession, setActiveSession] = useState<WorkbenchSession | null>(null);
  const [projectsLoading, setProjectsLoading] = useState<boolean>(false);
  const [projectDetailsLoading, setProjectDetailsLoading] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const projectsRequestIdRef = useRef<number>(0);
  const projectDetailsRequestIdRef = useRef<number>(0);
  const { t } = useTranslation(['workbench']);

  const panelPlaceholders: Record<MobileWorkbenchPanel, { title: string; label: string }> = {
    projects: {
      title: t('workbench:mobile.placeholders.projects.title'),
      label: t('workbench:mobile.placeholders.projects.label'),
    },
    terminal: {
      title: t('workbench:mobile.placeholders.terminal.title'),
      label: t('workbench:mobile.placeholders.terminal.label'),
    },
    files: {
      title: t('workbench:mobile.placeholders.files.title'),
      label: t('workbench:mobile.placeholders.files.label'),
    },
    git: {
      title: t('workbench:mobile.placeholders.git.title'),
      label: t('workbench:mobile.placeholders.git.label'),
    },
    worktrees: {
      title: t('workbench:mobile.placeholders.worktrees.title'),
      label: t('workbench:mobile.placeholders.worktrees.label'),
    },
    prompt: {
      title: t('workbench:mobile.placeholders.prompt.title'),
      label: t('workbench:mobile.placeholders.prompt.label'),
    },
    settings: {
      title: t('workbench:mobile.placeholders.settings.title'),
      label: t('workbench:mobile.placeholders.settings.label'),
    },
  };
  const placeholder = panelPlaceholders[panel];

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端进入 `/mobile` 后需要立即看到最近项目列表，也需要支持用户手动刷新。
   *
   * Code Logic（这个函数做什么）:
   *   调用 HTTP projects.list，使用递增 request id 丢弃旧响应，并更新项目列表加载态与错误态。
   */
  const loadProjects = useCallback(async (): Promise<void> => {
    const requestId = projectsRequestIdRef.current + 1;
    projectsRequestIdRef.current = requestId;
    setProjectsLoading(true);
    setError(null);

    try {
      const nextProjects = await httpWorkbenchTransport.projects.list();
      if (projectsRequestIdRef.current !== requestId) return;
      setProjects(nextProjects);
    } catch (reason) {
      if (projectsRequestIdRef.current !== requestId) return;
      setError(getErrorMessage(reason));
    } finally {
      if (projectsRequestIdRef.current === requestId) {
        setProjectsLoading(false);
      }
    }
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户选择本机项目后，移动端需要加载该项目的 worktree 和 terminal window；远端快捷方式当前只能提示不可用。
   *
   * Code Logic（这个函数做什么）:
   *   非 local 项目直接写入提示并让旧详情请求失效；local 项目并行请求 worktrees/sessions，选择主 worktree 与匹配 session，成功后切到 terminal 面板。
   */
  const selectProject = useCallback(async (project: WorkbenchProject): Promise<void> => {
    if (!canSelectMobileProject(project)) {
      projectDetailsRequestIdRef.current += 1;
      setProjectDetailsLoading(false);
      setError(t('workbench:mobile.projectPanel.remoteUnsupported'));
      return;
    }

    const requestId = projectDetailsRequestIdRef.current + 1;
    projectDetailsRequestIdRef.current = requestId;
    setProjectDetailsLoading(true);
    setError(null);
    setActiveProject(project);
    setWorktrees([]);
    setActiveWorktree(null);
    setSessions([]);
    setActiveSession(null);

    try {
      const [nextWorktrees, nextSessions] = await Promise.all([
        httpWorkbenchTransport.worktrees.list(project.id),
        httpWorkbenchTransport.sessions.list(project.id),
      ]);
      if (projectDetailsRequestIdRef.current !== requestId) return;

      const nextActiveWorktree = selectPreferredMobileWorktree(nextWorktrees);
      const nextActiveSession = selectPreferredMobileSession(
        nextSessions,
        nextActiveWorktree?.id ?? null,
      );

      setWorktrees(nextWorktrees);
      setActiveWorktree(nextActiveWorktree);
      setSessions(nextSessions);
      setActiveSession(nextActiveSession);
      setPanel('terminal');
    } catch (reason) {
      if (projectDetailsRequestIdRef.current !== requestId) return;
      setError(getErrorMessage(reason));
    } finally {
      if (projectDetailsRequestIdRef.current === requestId) {
        setProjectDetailsLoading(false);
      }
    }
  }, [t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   项目列表面板的刷新按钮需要触发异步加载，但按钮事件本身不消费 Promise。
   *
   * Code Logic（这个函数做什么）:
   *   调用 loadProjects 并显式丢弃 Promise，错误由 loadProjects 内部写入状态。
   */
  const handleRefreshProjects = useCallback((): void => {
    void loadProjects();
  }, [loadProjects]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户切换 worktree 后，移动端状态栏和终端面板应同步到同一 worktree 的优先 session。
   *
   * Code Logic（这个函数做什么）:
   *   写入 active worktree，并从当前 sessions 中选择匹配 session、running session 或首项。
   */
  const handleSelectWorktree = useCallback(
    (worktree: WorkbenchWorktree): void => {
      setActiveWorktree(worktree);
      setActiveSession(selectPreferredMobileSession(sessions, worktree.id));
    },
    [sessions],
  );

  useEffect(() => {
    void loadProjects();

    return () => {
      projectsRequestIdRef.current += 1;
      projectDetailsRequestIdRef.current += 1;
    };
  }, [loadProjects]);

  const panelContent =
    panel === 'projects' ? (
      <MobileProjectPanel
        projects={projects}
        activeProjectId={activeProject?.id ?? null}
        loading={projectsLoading}
        error={error}
        onSelect={selectProject}
        onRefresh={handleRefreshProjects}
      />
    ) : panel === 'worktrees' ? (
      <MobileWorktreePanel
        worktrees={worktrees}
        activeWorktreeId={activeWorktree?.id ?? null}
        onSelect={handleSelectWorktree}
      />
    ) : (
      <section className={styles.panel} aria-labelledby="mobile-panel-title">
        <div className={styles.panelHeader}>
          <p className={styles.panelKicker}>{t('workbench:mobile.kicker')}</p>
          <h1 id="mobile-panel-title">{placeholder.title}</h1>
        </div>
        {projectDetailsLoading ? (
          <p className={styles.panelState}>{t('workbench:loading')}</p>
        ) : (
          <div className={styles.placeholder}>{placeholder.label}</div>
        )}
        {error ? (
          <p className={styles.panelError}>
            <span>{t('workbench:mobile.projectPanel.error')}</span>
            <span>{error}</span>
          </p>
        ) : null}
      </section>
    );

  return (
    <MobileWorkbenchShell
      panel={panel}
      project={activeProject?.name ?? null}
      worktree={activeWorktree?.name ?? null}
      session={activeSession?.name ?? null}
      onPanelChange={setPanel}
    >
      {panelContent}
    </MobileWorkbenchShell>
  );
}
