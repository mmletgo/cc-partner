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

import { useCallback, useLayoutEffect, useMemo, useRef, useState, type DragEvent } from 'react';
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
import { moveProjectId, orderProjectsByIds } from '@/lib/workbenchRemoteProjects';
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
  const [dropIndicator, setDropIndicator] = useState<{
    projectId: string;
    position: 'before' | 'after';
  } | null>(null);
  const [previewOrderIds, setPreviewOrderIds] = useState<string[] | null>(null);
  /** 同步源 id：HTML5 dragover 必须同步 preventDefault，不能等 setState。 */
  const draggingProjectIdRef = useRef<string | null>(null);
  const previewOrderIdsRef = useRef<string[] | null>(null);
  const dragSucceededRef = useRef(false);
  const itemNodeRefs = useRef<Map<string, HTMLDivElement>>(new Map());
  const itemRectsRef = useRef<Map<string, DOMRect>>(new Map());
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



  const displayProjects = useMemo(() => {
    if (!previewOrderIds) return projects;
    return orderProjectsByIds(projects, previewOrderIds);
  }, [previewOrderIds, projects]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   列表顺序变化时用 FLIP 补间，让项目卡片滑动到新位置，而不是瞬间跳位。
   *
   * Code Logic（这个函数做什么）:
   *   layout 前缓存旧 rect，layout 后对每个 item 施加 inverse transform 再过渡回 0。
   */
  useLayoutEffect(() => {
    const nodes = itemNodeRefs.current;
    const prev = itemRectsRef.current;
    const nextRects = new Map<string, DOMRect>();
    for (const [id, node] of nodes) {
      nextRects.set(id, node.getBoundingClientRect());
    }
    if (prev.size > 0) {
      const reduceMotion =
        typeof window !== 'undefined' &&
        window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
      for (const [id, node] of nodes) {
        const oldRect = prev.get(id);
        const newRect = nextRects.get(id);
        if (!oldRect || !newRect) continue;
        const dx = oldRect.left - newRect.left;
        const dy = oldRect.top - newRect.top;
        if (Math.abs(dx) < 0.5 && Math.abs(dy) < 0.5) continue;
        if (reduceMotion) continue;
        node.animate(
          [
            { transform: `translate(${dx}px, ${dy}px)` },
            { transform: 'translate(0, 0)' },
          ],
          { duration: 180, easing: 'cubic-bezier(0.2, 0.8, 0.2, 1)' },
        );
      }
    }
    itemRectsRef.current = nextRects;
  }, [displayProjects]);

  const captureItemNode = useCallback((projectId: string, node: HTMLDivElement | null) => {
    if (!node) {
      itemNodeRefs.current.delete(projectId);
      return;
    }
    itemNodeRefs.current.set(projectId, node);
  }, []);

  const clearDragUi = useCallback(() => {
    draggingProjectIdRef.current = null;
    previewOrderIdsRef.current = null;
    setDraggingProjectId(null);
    setDropIndicator(null);
    setPreviewOrderIds(null);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   仅从手柄开始拖，避免点选/删除与拖拽冲突；list 层处理 over/drop，防止按钮吞事件。
   *
   * Code Logic（这个函数做什么）:
   *   同步写 ref（保证后续 dragover 立刻 preventDefault），再 setState 驱动 UI。
   */
  const handleHandleDragStart = useCallback(
    (event: DragEvent<HTMLSpanElement>, projectId: string) => {
      if (projectBusy) {
        event.preventDefault();
        return;
      }
      event.stopPropagation();
      event.dataTransfer.effectAllowed = 'move';
      try {
        event.dataTransfer.setData('text/plain', projectId);
        event.dataTransfer.setData('application/x-cc-partner-project-id', projectId);
      } catch {
        // 某些 WebView 对 setData 类型更严；text/plain 已尽力写入。
      }
      const card = event.currentTarget.closest(`.${styles.projectItem}`) as HTMLElement | null;
      if (card) {
        try {
          event.dataTransfer.setDragImage(card, 28, 24);
        } catch {
          // setDragImage 在部分环境不可用，忽略。
        }
      }
      const initialOrder = projects.map((project) => project.id);
      dragSucceededRef.current = false;
      draggingProjectIdRef.current = projectId;
      previewOrderIdsRef.current = initialOrder;
      setDraggingProjectId(projectId);
      setPreviewOrderIds(initialOrder);
      setDropIndicator(null);
    },
    [projectBusy, projects],
  );

  const resolveDropTarget = useCallback((clientY: number, listEl: HTMLElement, sourceId: string) => {
    const items = Array.from(listEl.querySelectorAll<HTMLElement>('[data-project-id]'));
    if (items.length === 0) return null;
    for (const item of items) {
      const id = item.dataset.projectId;
      if (!id || id === sourceId) continue;
      const rect = item.getBoundingClientRect();
      if (clientY < rect.top || clientY > rect.bottom) continue;
      const position: 'before' | 'after' =
        clientY < rect.top + rect.height / 2 ? 'before' : 'after';
      return { projectId: id, position };
    }
    let best: { projectId: string; position: 'before' | 'after'; dist: number } | null = null;
    for (const item of items) {
      const id = item.dataset.projectId;
      if (!id || id === sourceId) continue;
      const rect = item.getBoundingClientRect();
      const mid = rect.top + rect.height / 2;
      const dist = Math.abs(clientY - mid);
      const position: 'before' | 'after' = clientY < mid ? 'before' : 'after';
      if (!best || dist < best.dist) best = { projectId: id, position, dist };
    }
    return best ? { projectId: best.projectId, position: best.position } : null;
  }, []);

  const handleListDragOver = useCallback(
    (event: DragEvent<HTMLDivElement>) => {
      const sourceId = draggingProjectIdRef.current;
      if (!sourceId) return;
      // 必须同步 preventDefault，否则浏览器不会触发 drop。
      event.preventDefault();
      event.dataTransfer.dropEffect = 'move';
      const listEl =
        (event.currentTarget.closest(`.${styles.projectList}`) as HTMLElement | null) ??
        event.currentTarget;
      const target = resolveDropTarget(event.clientY, listEl, sourceId);
      if (!target) return;
      setDropIndicator(target);
      const base =
        previewOrderIdsRef.current ?? projects.map((project) => project.id);
      const next = moveProjectId(base, sourceId, target.projectId, target.position);
      if (next.join('\0') === base.join('\0')) return;
      previewOrderIdsRef.current = next;
      setPreviewOrderIds(next);
    },
    [projects, resolveDropTarget],
  );

  const handleListDrop = useCallback(
    (event: DragEvent<HTMLDivElement>) => {
      const sourceFromRef = draggingProjectIdRef.current;
      if (!sourceFromRef) return;
      event.preventDefault();
      event.stopPropagation();
      let sourceId = sourceFromRef;
      try {
        sourceId =
          event.dataTransfer.getData('application/x-cc-partner-project-id') ||
          event.dataTransfer.getData('text/plain') ||
          sourceFromRef;
      } catch {
        sourceId = sourceFromRef;
      }
      // drop 可能落在 list 或 item 上；resolve 始终相对整个列表容器。
      const listEl =
        (event.currentTarget.closest(`.${styles.projectList}`) as HTMLElement | null) ??
        event.currentTarget;
      const target = resolveDropTarget(event.clientY, listEl, sourceId);
      const base =
        previewOrderIdsRef.current ?? projects.map((project) => project.id);
      const next =
        target && sourceId
          ? moveProjectId(base, sourceId, target.projectId, target.position)
          : base;
      dragSucceededRef.current = true;
      clearDragUi();
      const unchanged =
        next.length === projects.length &&
        next.every((id, index) => projects[index]?.id === id);
      if (!unchanged) {
        void reorderProjects(next);
      }
    },
    [clearDragUi, projects, reorderProjects, resolveDropTarget],
  );

  const handleHandleDragEnd = useCallback(() => {
    // drop 后仍会触发 dragend；成功路径已 clear，这里只处理取消/失败。
    if (!dragSucceededRef.current) {
      clearDragUi();
    } else {
      draggingProjectIdRef.current = null;
      setDraggingProjectId(null);
      setDropIndicator(null);
    }
  }, [clearDragUi]);

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

      <div
        className={styles.projectList}
        data-dragging={draggingProjectId ? true : undefined}
        onDragEnter={(event) => {
          if (draggingProjectIdRef.current) event.preventDefault();
        }}
        onDragOver={handleListDragOver}
        onDrop={handleListDrop}
      >
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
        {displayProjects.map((project) => {
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
              ref={(node) => captureItemNode(project.id, node)}
              className={styles.projectItem}
              data-project-id={project.id}
              data-active={isActive || undefined}
              data-dragging={draggingProjectId === project.id || undefined}
              data-drop-before={
                dropIndicator?.projectId === project.id && dropIndicator.position === 'before'
                  ? true
                  : undefined
              }
              data-drop-after={
                dropIndicator?.projectId === project.id && dropIndicator.position === 'after'
                  ? true
                  : undefined
              }
              onDragOver={handleListDragOver}
              onDrop={handleListDrop}
            >
              <span
                className={styles.dragHandle}
                draggable={!projectBusy}
                title={t('workbench:projectRail.dragHandleAria')}
                aria-label={t('workbench:projectRail.dragHandleAria')}
                onDragStart={(event) => handleHandleDragStart(event, project.id)}
                onDragEnd={handleHandleDragEnd}
              >
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
