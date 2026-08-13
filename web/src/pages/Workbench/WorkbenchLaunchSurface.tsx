/**
 * Workbench 启动表面（零项目聚焦 CTA / 有项目未选中「继续工作」）。
 *
 * Business Logic（为什么需要这个组件）:
 *   `/workbench` 在无 active project 时不应渲染完整终端 chrome；零项目只展示三个聚焦动作，
 *   有项目未选中时展示「继续工作」四 section 摘要 + Attention 计数，并复用既有 deep link。
 *   从 Workbench.tsx 抽出以保持页面 ≤1200 行。
 *
 * Code Logic（这个组件做什么）:
 *   mode=empty：本机添加 / 远端连接 / 检查 tmux；mode=continue：Attention 摘要 + 四 Card section，
 *   独立 loading/error/empty/stale 渲染；点击走 selectProject / deep link / 路由跳转。
 */

import { useCallback, useRef, useState } from 'react';
import type { ReactElement, ReactNode, RefObject } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Button, Card, Dialog, Pill, StatusMessage } from '@/components/primitives';
import { WorkbenchRemoteProjectPicker } from '@/components/domain/WorkbenchRemoteProjectPicker';
import { useAttention } from '@/hooks/useAttention';
import { useWorkbenchDependency } from '@/hooks/workbenchDependencyContext';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
import type {
  WorkbenchLaunchProject,
  WorkbenchLaunchSession,
  WorkbenchLaunchTask,
  WorkbenchLaunchTransfer,
} from '@/lib/types';
import { buildWorkbenchDeepLink } from './workbenchDeepLink';
import type {
  WorkbenchLaunchResource,
  WorkbenchLaunchSummaryState,
} from './workbenchLaunchState';
import styles from './Workbench.module.css';

export type WorkbenchLaunchSurfaceMode = 'empty' | 'continue';

/**
 * 零项目空态动作回调。远端选择 Dialog 仍由 LaunchSurface 托管；
 * onConnectRemote 由表面层注入为「打开 picker」，页面层只提供本机添加与 tmux 检查。
 */
export interface WorkbenchEmptyStateActions {
  /** 添加本机项目（复用 projects context 的 chooseAndAddProject）。 */
  onAddLocal: () => void;
  /** 打开远端项目选择器（由 LaunchSurface 注入 open-picker 回调）。 */
  onConnectRemote: () => void;
  /** 检查 tmux 依赖并导航 Settings 依赖 tab。 */
  onCheckTmux: () => void;
}

/** 页面层注入的空态动作（不含远端 picker 打开，picker 由 LaunchSurface 托管）。 */
export type WorkbenchEmptyStatePageActions = Pick<
  WorkbenchEmptyStateActions,
  'onAddLocal' | 'onCheckTmux'
>;

export interface WorkbenchLaunchSurfaceProps {
  mode: WorkbenchLaunchSurfaceMode;
  launchSummary: WorkbenchLaunchSummaryState;
  onRefreshLaunchSummary: () => void;
  /**
   * 零项目聚焦空态动作回调；mode=empty 时由页面注入 add-local / check-tmux。
   * 空态纯视图只消费回调，不直接触发 projects/dependency mutation API。
   */
  emptyActions?: WorkbenchEmptyStatePageActions;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   section 在 ready / error+cached 时都可能有列表数据需要展示。
 *
 * Code Logic（这个函数做什么）:
 *   ready → value；error 且有 cached → cached；否则 null。
 */
function sectionListValue<T>(resource: WorkbenchLaunchResource<T[]>): T[] | null {
  if (resource.kind === 'ready') return resource.value;
  if (resource.kind === 'error' && resource.cached) return resource.cached;
  return null;
}

function sectionIsStale<T>(resource: WorkbenchLaunchResource<T>): boolean {
  return resource.kind === 'ready' && resource.stale;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   单个 launch section 需要独立 loading / empty / error / list UI。
 *
 * Code Logic（这个组件做什么）:
 *   按 resource kind 渲染状态文案或列表；空数组走真实 empty CTA。
 */
function LaunchSectionCard<T>({
  title,
  resource,
  emptyLabel,
  emptyAction,
  renderItem,
  getKey,
}: {
  title: string;
  resource: WorkbenchLaunchResource<T[]>;
  emptyLabel: string;
  emptyAction?: ReactNode;
  renderItem: (item: T) => ReactNode;
  getKey: (item: T) => string;
}): ReactElement {
  const { t } = useTranslation(['workbench']);
  const list = sectionListValue(resource);
  const stale = sectionIsStale(resource);

  return (
    <Card className={styles.launchSectionCard} padding="md" variant="outlined">
      <Card.Header className={styles.launchSectionHeader}>
        <h2 className={styles.launchSectionTitle}>{title}</h2>
        {stale ? (
          <Pill tone="warn">{t('workbench:launch.stale')}</Pill>
        ) : null}
      </Card.Header>
      <Card.Body className={styles.launchSectionBody}>
        {resource.kind === 'loading' ? (
          <p className={styles.launchMuted}>{t('workbench:launch.loading')}</p>
        ) : null}
        {resource.kind === 'error' && !list ? (
          <StatusMessage tone="danger">
            {t('workbench:launch.error', { message: resource.message })}
          </StatusMessage>
        ) : null}
        {resource.kind === 'error' && list ? (
          <StatusMessage tone="warn" className={styles.launchSectionNotice}>
            {t('workbench:launch.error', { message: resource.message })}
          </StatusMessage>
        ) : null}
        {list && list.length === 0 ? (
          <div className={styles.launchEmptyBlock}>
            <p className={styles.launchMuted}>{emptyLabel}</p>
            {emptyAction}
          </div>
        ) : null}
        {list && list.length > 0 ? (
          <ul className={styles.launchItemList}>
            {list.map((item) => (
              <li key={getKey(item)} className={styles.launchItem}>
                {renderItem(item)}
              </li>
            ))}
          </ul>
        ) : null}
      </Card.Body>
    </Card>
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   零项目时只能展示聚焦 CTA，不能夹带禁用 toolbar / 空终端 / inspector。
 *
 * Code Logic（这个组件做什么）:
 *   纯展示：一句解释 + 三个动作按钮；全部交互走 props 回调，不读 projects/dependency context。
 */
function WorkbenchEmptyStateView({
  onAddLocal,
  onConnectRemote,
  onCheckTmux,
  addLocalButtonRef,
}: WorkbenchEmptyStateActions & {
  addLocalButtonRef: RefObject<HTMLButtonElement | null>;
}): ReactElement {
  const { t } = useTranslation(['workbench']);
  return (
    <main className={styles.launchEmptyMain}>
      <h1 className={styles.launchTitle}>{t('workbench:launch.emptyTitle')}</h1>
      <p className={styles.launchExplanation}>{t('workbench:launch.emptyExplanation')}</p>
      <div className={styles.launchEmptyActions}>
        <Button ref={addLocalButtonRef} variant="primary" onClick={onAddLocal}>
          {t('workbench:launch.addLocal')}
        </Button>
        <Button variant="secondary" onClick={onConnectRemote}>
          {t('workbench:launch.connectRemote')}
        </Button>
        <Button variant="ghost" onClick={onCheckTmux}>
          {t('workbench:launch.checkTmux')}
        </Button>
      </div>
    </main>
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench 在无 active project 时的主表面：零项目聚焦 CTA 或「继续工作」摘要。
 *
 * Code Logic（这个组件做什么）:
 *   mode=empty：只渲染纯空态视图 + 远端选择 Dialog，动作来自 emptyActions 回调；
 *   mode=continue：组合 Attention / launchSummary section 与 deep link 导航。
 */
export function WorkbenchLaunchSurface({
  mode,
  launchSummary,
  onRefreshLaunchSummary,
  emptyActions,
}: WorkbenchLaunchSurfaceProps): ReactElement {
  const { t } = useTranslation(['workbench', 'attention']);
  const navigate = useNavigate();
  const { chooseAndAddProject, openRemoteProject, selectProject, projects } =
    useWorkbenchProjects();
  const { check: checkDependency } = useWorkbenchDependency();
  const attention = useAttention();

  const addLocalButtonRef = useRef<HTMLButtonElement>(null);
  const [remotePickerOpen, setRemotePickerOpen] = useState(false);
  const [remoteOpenBusy, setRemoteOpenBusy] = useState(false);

  /**
   * Business Logic（为什么需要这个函数）:
   *   继续工作 section 空态仍需要「添加本机项目」快捷入口。
   *
   * Code Logic（这个函数做什么）:
   *   调用 chooseAndAddProject；成功后由 projects context 选中项目。
   */
  const handleAddLocal = useCallback(() => {
    void chooseAndAddProject();
  }, [chooseAndAddProject]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   连接远端项目必须复用现有 RemoteProjectPicker，不新增 API。
   *
   * Code Logic（这个函数做什么）:
   *   打开远端 Dialog；纯空态 onConnectRemote 固定绑定此函数。
   */
  const handleOpenRemotePicker = useCallback(() => {
    setRemoteOpenBusy(false);
    setRemotePickerOpen(true);
  }, []);

  const closeRemotePicker = useCallback(
    (options?: { force?: boolean }) => {
      if (remoteOpenBusy && !options?.force) return;
      setRemotePickerOpen(false);
      setRemoteOpenBusy(false);
      window.setTimeout(() => addLocalButtonRef.current?.focus(), 0);
    },
    [remoteOpenBusy],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   检查 tmux 依赖：触发 recheck 并导航 Settings 依赖 tab。
   *
   * Code Logic（这个函数做什么）:
   *   void checkDependency()；navigate `/settings?tab=dependencies`。
   */
  const handleCheckTmux = useCallback(() => {
    void checkDependency();
    navigate('/settings?tab=dependencies');
  }, [checkDependency, navigate]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   点击最近项目应激活该项目进入正常 Workbench chrome。
   *
   * Code Logic（这个函数做什么）:
   *   在 projects 列表中查找并 selectProject；找不到则忽略。
   */
  const handleSelectLaunchProject = useCallback(
    (item: WorkbenchLaunchProject) => {
      const target = projects.find((project) => project.id === item.id);
      if (!target) return;
      void selectProject(target);
    },
    [projects, selectProject],
  );

  const handleOpenSession = useCallback(
    (item: WorkbenchLaunchSession) => {
      navigate(
        buildWorkbenchDeepLink({
          projectId: item.projectId,
          worktreeId: item.worktreeId ?? null,
          sessionId: item.id,
        }),
      );
    },
    [navigate],
  );

  const handleOpenTask = useCallback(
    (item: WorkbenchLaunchTask) => {
      navigate(
        buildWorkbenchDeepLink({
          projectId: item.projectId,
          worktreeId: null,
          sessionId: null,
          view: 'automation',
          taskId: item.id,
        }),
      );
    },
    [navigate],
  );

  const attentionCounts = attention.snapshot?.counts;
  const attentionTotal = attentionCounts?.total ?? 0;

  if (mode === 'empty') {
    // 纯空态只吃回调：本机/tmux 优先页面注入；远端连接固定打开本表面托管的 picker。
    const emptyViewActions: WorkbenchEmptyStateActions = {
      onAddLocal: emptyActions?.onAddLocal ?? handleAddLocal,
      onConnectRemote: handleOpenRemotePicker,
      onCheckTmux: emptyActions?.onCheckTmux ?? handleCheckTmux,
    };
    return (
      <div className={styles.launchPage} data-testid="workbench-launch-empty">
        <WorkbenchEmptyStateView
          onAddLocal={emptyViewActions.onAddLocal}
          onConnectRemote={emptyViewActions.onConnectRemote}
          onCheckTmux={emptyViewActions.onCheckTmux}
          addLocalButtonRef={addLocalButtonRef}
        />
        <Dialog
          open={remotePickerOpen}
          titleId="workbench-launch-remote-picker-title"
          onClose={() => {
            closeRemotePicker();
          }}
          closeOnEscape={!remoteOpenBusy}
          closeOnBackdrop={!remoteOpenBusy}
          className={styles.launchRemoteDialog}
        >
          <h2 id="workbench-launch-remote-picker-title" className="sr-only">
            {t('workbench:remoteProjectPicker.title')}
          </h2>
          <WorkbenchRemoteProjectPicker
            openProject={openRemoteProject}
            onCancel={closeRemotePicker}
            onOpenBusyChange={setRemoteOpenBusy}
            onProjectOpened={() => {
              closeRemotePicker({ force: true });
            }}
          />
        </Dialog>
      </div>
    );
  }

  return (
    <div className={styles.launchPage} data-testid="workbench-launch-continue">
      <main className={styles.launchContinueMain}>
        <header className={styles.launchHeader}>
          <div>
            <h1 className={styles.launchTitle}>{t('workbench:launch.title')}</h1>
            <p className={styles.launchExplanation}>{t('workbench:launch.emptyExplanation')}</p>
          </div>
          <Button variant="secondary" size="sm" onClick={onRefreshLaunchSummary}>
            {t('workbench:launch.refresh')}
          </Button>
        </header>

        <Card className={styles.launchAttentionCard} padding="md" variant="outlined">
          <Card.Body>
            {attentionTotal > 0 && attentionCounts ? (
              <div className={styles.launchAttentionRow}>
                <p className={styles.launchAttentionText}>
                  {t('workbench:launch.attentionSummary', {
                    total: attentionCounts.total,
                    decision: attentionCounts.decision,
                    blocked: attentionCounts.blocked,
                    environment: attentionCounts.environment,
                  })}
                </p>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => {
                    navigate('/attention');
                  }}
                >
                  {t('workbench:launch.attentionOpen')}
                </Button>
              </div>
            ) : (
              <p className={styles.launchMuted}>{t('workbench:launch.attentionEmpty')}</p>
            )}
          </Card.Body>
        </Card>

        <div className={styles.launchSectionsGrid}>
          <LaunchSectionCard<WorkbenchLaunchProject>
            title={t('workbench:launch.sections.projects')}
            resource={launchSummary.projects}
            emptyLabel={t('workbench:launch.empty.projects')}
            emptyAction={
              <Button variant="secondary" size="sm" onClick={handleAddLocal}>
                {t('workbench:launch.addLocal')}
              </Button>
            }
            getKey={(item) => item.id}
            renderItem={(item) => (
              <button
                type="button"
                className={styles.launchItemButton}
                onClick={() => {
                  handleSelectLaunchProject(item);
                }}
              >
                <span className={styles.launchItemTitle}>{item.name}</span>
                <span className={styles.launchItemMeta}>
                  {item.deviceName} · {item.path}
                </span>
                <Pill tone="neutral">
                  {item.kind === 'remote'
                    ? t('workbench:launch.item.remote')
                    : t('workbench:launch.item.local')}
                </Pill>
              </button>
            )}
          />

          <LaunchSectionCard<WorkbenchLaunchSession>
            title={t('workbench:launch.sections.sessions')}
            resource={launchSummary.sessions}
            emptyLabel={t('workbench:launch.empty.sessions')}
            getKey={(item) => item.id}
            renderItem={(item) => (
              <button
                type="button"
                className={styles.launchItemButton}
                onClick={() => {
                  handleOpenSession(item);
                }}
              >
                <span className={styles.launchItemTitle}>{item.name}</span>
                <span className={styles.launchItemMeta}>
                  {item.projectName} · {item.status}
                </span>
              </button>
            )}
          />

          <LaunchSectionCard<WorkbenchLaunchTask>
            title={t('workbench:launch.sections.tasks')}
            resource={launchSummary.tasks}
            emptyLabel={t('workbench:launch.empty.tasks')}
            getKey={(item) => item.id}
            renderItem={(item) => (
              <button
                type="button"
                className={styles.launchItemButton}
                onClick={() => {
                  handleOpenTask(item);
                }}
              >
                <span className={styles.launchItemTitle}>{item.title}</span>
                <span className={styles.launchItemMeta}>
                  {(item.projectName ?? item.projectId) +
                    ' · ' +
                    item.workflowState +
                    ' / ' +
                    item.runState}
                </span>
              </button>
            )}
          />

          <LaunchSectionCard<WorkbenchLaunchTransfer>
            title={t('workbench:launch.sections.transfers')}
            resource={launchSummary.transfers}
            emptyLabel={t('workbench:launch.empty.transfers')}
            emptyAction={
              <Button
                variant="secondary"
                size="sm"
                onClick={() => {
                  navigate('/transfer');
                }}
              >
                {t('workbench:launch.openTransfer')}
              </Button>
            }
            getKey={(item) => item.id}
            renderItem={(item) => (
              <button
                type="button"
                className={styles.launchItemButton}
                onClick={() => {
                  navigate('/transfer');
                }}
              >
                <span className={styles.launchItemTitle}>{item.filename}</span>
                <span className={styles.launchItemMeta}>
                  {item.direction} · {item.status}
                </span>
              </button>
            )}
          />
        </div>
      </main>

      <Dialog
        open={remotePickerOpen}
        titleId="workbench-launch-remote-picker-title-continue"
        onClose={() => {
          closeRemotePicker();
        }}
        closeOnEscape={!remoteOpenBusy}
        closeOnBackdrop={!remoteOpenBusy}
        className={styles.launchRemoteDialog}
      >
        <h2 id="workbench-launch-remote-picker-title-continue" className="sr-only">
          {t('workbench:remoteProjectPicker.title')}
        </h2>
        <WorkbenchRemoteProjectPicker
          openProject={openRemoteProject}
          onCancel={closeRemotePicker}
          onOpenBusyChange={setRemoteOpenBusy}
          onProjectOpened={() => {
            closeRemotePicker({ force: true });
          }}
        />
      </Dialog>
    </div>
  );
}
