/**
 * WorkbenchProjectRail（侧栏项目文件夹入口）
 *
 * Business Logic（为什么需要这个组件）:
 *   项目文件夹列表是进入工作台的主要入口，不需要再占用一个独立导航菜单项。
 *   分区标题、空态说明与本机/局域网 CTA 提升旗舰功能可发现性；状态不只靠颜色。
 *
 * Code Logic（这个组件做什么）:
 *   渲染设置菜单项下方的项目列表、window/pane 统计、本机/远端添加入口和项目移除操作；
 *   空态直接暴露 chooseAndAddProject / 远端选择器（复用既有回调，不新增项目 API）；
 *   点击项目后选择项目并跳转 `/workbench`，保持 deep link 语义。
 *   来源选择与远端项目选择统一走共享 Dialog（portal / focus trap / Escape / backdrop）。
 */

import { useCallback, useMemo, useRef, useState, type DragEvent } from 'react';
import { Link, useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Button, Dialog } from '@/components/primitives';
import { DevicesIcon, FolderIcon, PlusIcon, SyncIcon, XIcon } from '@/lib/icons';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
import { useLanAgentFleet } from '@/hooks/useLanAgentFleet';
import { EMPTY_PROJECT_SESSION_STATS } from '@/lib/workbenchProjectStats';
import { fleetExceptionCount } from '@/lib/types/lanFleet';
import type { LanFleetDeviceSummary, LanFleetProjectSummary } from '@/lib/types/lanFleet';
import { WorkbenchRemoteProjectPicker } from '@/components/domain/WorkbenchRemoteProjectPicker';
import styles from './WorkbenchProjectRail.module.css';

/**
 * Business Logic（为什么需要这个组件）:
 *   用户应能从任意页面选择本机或局域网远端项目文件夹进入 Workbench。
 *
 * Code Logic（这个组件做什么）:
 *   使用共享 Workbench 项目上下文渲染项目列表、terminal window/pane 统计和添加来源选择，并用 React Router 导航到 `/workbench`。
 */
export function WorkbenchProjectRail() {
  const { t } = useTranslation(['workbench']);
  const navigate = useNavigate();
  const addProjectButtonRef = useRef<HTMLButtonElement>(null);
  const [sourcePickerOpen, setSourcePickerOpen] = useState<boolean>(false);
  const [remotePickerOpen, setRemotePickerOpen] = useState<boolean>(false);
  const [remoteOpenBusy, setRemoteOpenBusy] = useState<boolean>(false);
  const [draggingProjectId, setDraggingProjectId] = useState<string | null>(null);
  const [dropTargetId, setDropTargetId] = useState<string | null>(null);
  const {
    projects,
    activeProjectId,
    projectsLoading,
    projectBusy,
    projectError,
    projectSessionStats,
    loadProjects,
    chooseAndAddProject,
    openRemoteProject,
    selectProject,
    removeProject,
    reorderProjects,
  } = useWorkbenchProjects();

  const { projectSummaries, snapshot: fleetSnapshot } = useLanAgentFleet({ enabled: true });

  /**
   * Business Logic（为什么需要这个映射）:
   *   Rail 需要按 project 查找 device reachability（offline 文本）。
   *
   * Code Logic（这个函数做什么）:
   *   projectId → 所属 device summary。
   */
  const deviceByProjectId = useMemo(() => {
    const map: Record<string, LanFleetDeviceSummary> = {};
    if (!fleetSnapshot) return map;
    for (const device of fleetSnapshot.devices) {
      for (const project of device.projects) {
        map[project.projectId] = device;
      }
    }
    return map;
  }, [fleetSnapshot]);

  const sectionTitle = t('workbench:projectRail.sectionTitle');

  /**
   * 关闭远端项目选择 Dialog。
   *
   * Business Logic（为什么需要这个函数）:
   *   打开远端项目进行中时不应被 Esc/遮罩打断；完成后或强制关闭时回到添加按钮。
   *
   * Code Logic（这个函数做什么）:
   *   busy 且非 force 时 no-op；否则关闭并清理 busy，并聚焦添加按钮。
   */
  const closeRemotePicker = useCallback((options?: { force?: boolean }) => {
    if (remoteOpenBusy && !options?.force) return;
    setRemotePickerOpen(false);
    setRemoteOpenBusy(false);
    window.setTimeout(() => addProjectButtonRef.current?.focus(), 0);
  }, [remoteOpenBusy]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   本机项目 CTA（空态按钮与来源弹层）共用同一添加流程。
   *
   * Code Logic（这个函数做什么）:
   *   关闭来源弹层后调用 chooseAndAddProject；成功则导航 /workbench。
   */
  const handleAddLocalProject = useCallback(() => {
    setSourcePickerOpen(false);
    void chooseAndAddProject().then((project) => {
      if (project) navigate('/workbench');
    });
  }, [chooseAndAddProject, navigate]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   局域网项目 CTA 必须复用现有远端选择器，不新增项目 API。
   *
   * Code Logic（这个函数做什么）:
   *   关闭来源弹层并打开 remote picker。
   */
  const handleOpenRemotePicker = useCallback(() => {
    setSourcePickerOpen(false);
    setRemoteOpenBusy(false);
    setRemotePickerOpen(true);
  }, []);

  /**
   * 关闭来源选择 Dialog，并尝试把焦点还回添加按钮。
   *
   * Business Logic（为什么需要这个函数）:
   *   用户取消选择本机/远端来源后应回到触发入口，便于继续操作。
   *
   * Code Logic（这个函数做什么）:
   *   setSourcePickerOpen(false)；下一帧 focus 添加按钮。
   */
  const closeSourcePicker = useCallback(() => {
    setSourcePickerOpen(false);
    window.setTimeout(() => addProjectButtonRef.current?.focus(), 0);
  }, []);


  /**
   * Business Logic（为什么需要这个函数）:
   *   侧栏拖拽需要在拖动开始时标记源项目，并在放置时重排 orderedIds。
   *
   * Code Logic（这个函数做什么）:
   *   HTML5 DnD：start 记 source；over 记 hover target；drop 时按 id 列表重排并调用 reorderProjects。
   */
  const handleProjectDragStart = useCallback((event: DragEvent<HTMLDivElement>, projectId: string) => {
    event.dataTransfer.effectAllowed = 'move';
    event.dataTransfer.setData('text/plain', projectId);
    setDraggingProjectId(projectId);
  }, []);

  const handleProjectDragOver = useCallback((event: DragEvent<HTMLDivElement>, projectId: string) => {
    event.preventDefault();
    event.dataTransfer.dropEffect = 'move';
    setDropTargetId(projectId);
  }, []);

  const handleProjectDragEnd = useCallback(() => {
    setDraggingProjectId(null);
    setDropTargetId(null);
  }, []);

  const handleProjectDrop = useCallback(
    (event: DragEvent<HTMLDivElement>, targetProjectId: string) => {
      event.preventDefault();
      const sourceId = event.dataTransfer.getData('text/plain') || draggingProjectId;
      setDraggingProjectId(null);
      setDropTargetId(null);
      if (!sourceId || sourceId === targetProjectId) return;
      const ids = projects.map((project) => project.id);
      const from = ids.indexOf(sourceId);
      const to = ids.indexOf(targetProjectId);
      if (from < 0 || to < 0) return;
      const next = [...ids];
      const [moved] = next.splice(from, 1);
      next.splice(to, 0, moved);
      void reorderProjects(next);
    },
    [draggingProjectId, projects, reorderProjects],
  );

  return (
    <section className={styles.rail} aria-label={sectionTitle}>
      <div className={styles.header}>
        <h2 className={styles.title}>{sectionTitle}</h2>
        <div className={styles.actions}>
          <Link
            to="/workbench/fleet"
            className={styles.fleetLink}
            aria-label={t('workbench:projectRail.fleetLinkAria')}
          >
            {t('workbench:projectRail.fleetLink')}
          </Link>
          <Button
            variant="icon"
            icon={<SyncIcon />}
            title={t('workbench:refresh')}
            aria-label={t('workbench:refresh')}
            onClick={() => void loadProjects()}
          />
          <Button
            ref={addProjectButtonRef}
            variant="icon"
            icon={<PlusIcon />}
            title={t('workbench:addProject')}
            aria-label={t('workbench:addProject')}
            aria-haspopup="dialog"
            aria-expanded={sourcePickerOpen || remotePickerOpen}
            loading={projectBusy}
            onClick={() => setSourcePickerOpen((open) => !open)}
          />
        </div>
      </div>

      {projectError ? <div className={styles.errorBox}>{projectError}</div> : null}

      <div className={styles.projectList}>
        {projectsLoading ? <div className={styles.muted}>{t('workbench:loading')}</div> : null}
        {!projectsLoading && projects.length === 0 ? (
          <div className={styles.emptyProject}>
            <FolderIcon />
            <span className={styles.emptyTitle}>{t('workbench:emptyProjects')}</span>
            <p className={styles.emptyExplanation}>
              {t('workbench:projectRail.emptyExplanation')}
            </p>
            <div className={styles.emptyActions}>
              <button
                type="button"
                className={styles.emptyCta}
                onClick={handleAddLocalProject}
                disabled={projectBusy}
              >
                {t('workbench:projectRail.addLocalCta')}
              </button>
              <button
                type="button"
                className={styles.emptyCta}
                onClick={handleOpenRemotePicker}
                disabled={projectBusy}
              >
                {t('workbench:projectRail.addRemoteCta')}
              </button>
            </div>
          </div>
        ) : null}
        {projects.map((project) => {
          const stats = projectSessionStats[project.id] ?? EMPTY_PROJECT_SESSION_STATS;
          const windowCountLabel = t('workbench:projectWindowCount', {
            count: stats.windowCount,
          });
          const paneCountLabel = t('workbench:projectPaneCount', {
            count: stats.paneCount,
          });
          const isActive = project.id === activeProjectId;
          const statusLabel = isActive
            ? t('workbench:projectRail.statusActive')
            : t('workbench:projectRail.statusInactive');
          const fleetProject: LanFleetProjectSummary | undefined =
            projectSummaries[project.id];
          const fleetDevice = deviceByProjectId[project.id];
          const exceptionCount = fleetProject
            ? fleetExceptionCount(fleetProject.agentCounts)
            : 0;
          const workingCount = fleetProject?.agentCounts.working ?? 0;
          const offline = fleetDevice?.reachability === 'offline';
          const unsupported = fleetDevice?.reachability === 'unsupported';
          const cached = fleetDevice?.freshness === 'cached';
          const agentHintParts: string[] = [];
          if (workingCount > 0) {
            agentHintParts.push(
              t('workbench:projectRail.agentWorkingHint', { count: workingCount }),
            );
          }
          if (offline) agentHintParts.push(t('workbench:projectRail.deviceOffline'));
          if (cached) agentHintParts.push(t('workbench:projectRail.deviceCached'));
          if (unsupported) {
            agentHintParts.push(t('workbench:projectRail.deviceUnsupported'));
          }
          const agentHint = agentHintParts.join(' · ');
          return (
            <div
              key={project.id}
              className={styles.projectItem}
              data-active={isActive || undefined}
              data-dragging={draggingProjectId === project.id || undefined}
              data-drop-target={
                dropTargetId === project.id && draggingProjectId !== project.id
                  ? true
                  : undefined
              }
              draggable={!projectBusy}
              onDragStart={(event) => handleProjectDragStart(event, project.id)}
              onDragOver={(event) => handleProjectDragOver(event, project.id)}
              onDrop={(event) => handleProjectDrop(event, project.id)}
              onDragEnd={handleProjectDragEnd}
            >
              <span className={styles.dragHandle} aria-hidden="true" title={t('workbench:projectRail.dragHandleAria')}>
                ⋮⋮
              </span>
              <button
                type="button"
                className={styles.projectSelectButton}
                title={agentHint || undefined}
                onClick={() => {
                  void selectProject(project).then(() => navigate('/workbench'));
                }}
              >
                <span className={styles.projectText}>
                  <span className={styles.projectNameRow}>
                    <span className={styles.projectName}>{project.name}</span>
                    {fleetProject ? (
                      <span
                        className={styles.agentStatusDot}
                        data-tone={
                          exceptionCount > 0
                            ? 'exception'
                            : workingCount > 0
                              ? 'working'
                              : 'idle'
                        }
                        aria-hidden="true"
                      />
                    ) : null}
                  </span>
                  <span className={styles.projectPath}>{project.path}</span>
                  <span className={styles.projectMeta}>
                    <span className={styles.projectDevice}>
                      {project.kind === 'remote' ? (
                        <span className={styles.remoteBadge}>{t('workbench:remoteBadge')}</span>
                      ) : null}
                      <span>{project.deviceName}</span>
                      {offline ? (
                        <span className={styles.offlineText}>
                          {t('workbench:projectRail.deviceOffline')}
                        </span>
                      ) : null}
                      {cached && !offline ? (
                        <span className={styles.offlineText}>
                          {t('workbench:projectRail.deviceCached')}
                        </span>
                      ) : null}
                    </span>
                    <span
                      className={styles.projectStats}
                      aria-label={`${windowCountLabel}, ${paneCountLabel}${
                        agentHint ? `, ${agentHint}` : ''
                      }`}
                    >
                      <span>{windowCountLabel}</span>
                      <span aria-hidden="true">·</span>
                      <span>{paneCountLabel}</span>
                    </span>
                  </span>
                  <span className={styles.projectStatusText}>{statusLabel}</span>
                </span>
              </button>
              <span
                className={styles.projectStatusDot}
                data-active={isActive || undefined}
                aria-hidden="true"
              />
              {exceptionCount > 0 ? (
                <Link
                  to={`/attention?projectId=${encodeURIComponent(project.id)}`}
                  className={styles.exceptionBadge}
                  aria-label={t('workbench:projectRail.agentExceptionBadge', {
                    count: exceptionCount,
                  })}
                >
                  {exceptionCount}
                </Link>
              ) : null}
              <Button
                className={styles.projectRemoveButton}
                variant="icon"
                icon={<XIcon />}
                title={t('workbench:removeProject')}
                aria-label={t('workbench:removeProject')}
                onClick={() => void removeProject(project.id)}
              />
            </div>
          );
        })}
      </div>

      <Dialog
        open={sourcePickerOpen}
        titleId="workbench-source-picker-title"
        onClose={closeSourcePicker}
        className={styles.sourcePopover}
      >
        <h2 id="workbench-source-picker-title" className="sr-only">
          {t('workbench:addProject')}
        </h2>
        <button
          type="button"
          className={styles.sourceOption}
          onClick={handleAddLocalProject}
        >
          <FolderIcon />
          <span>
            <span>{t('workbench:projectSources.local')}</span>
            <span>{t('workbench:projectSources.localDescription')}</span>
          </span>
        </button>
        <button
          type="button"
          className={styles.sourceOption}
          onClick={handleOpenRemotePicker}
        >
          <DevicesIcon />
          <span>
            <span>{t('workbench:projectSources.remote')}</span>
            <span>{t('workbench:projectSources.remoteDescription')}</span>
          </span>
        </button>
      </Dialog>

      <Dialog
        open={remotePickerOpen}
        titleId="workbench-remote-picker-title"
        onClose={() => {
          closeRemotePicker();
        }}
        closeOnEscape={!remoteOpenBusy}
        closeOnBackdrop={!remoteOpenBusy}
        className={styles.modalDialog}
      >
        <h2 id="workbench-remote-picker-title" className="sr-only">
          {t('workbench:remoteProjectPicker.title')}
        </h2>
        <WorkbenchRemoteProjectPicker
          openProject={openRemoteProject}
          onCancel={closeRemotePicker}
          onOpenBusyChange={setRemoteOpenBusy}
          onProjectOpened={() => {
            closeRemotePicker({ force: true });
            navigate('/workbench');
          }}
        />
      </Dialog>
    </section>
  );
}
