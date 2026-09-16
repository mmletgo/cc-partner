/**
 * WorkbenchProjectRail（侧栏项目文件夹入口）
 *
 * Business Logic（为什么需要这个组件）:
 *   项目文件夹列表是进入工作台的主要入口，不需要再占用一个独立导航菜单项。
 *   分区标题、空态说明与本机/局域网 CTA 提升旗舰功能可发现性；状态不只靠颜色。
 *
 * Code Logic（这个组件做什么）:
 *   渲染设置菜单项下方的项目列表、window/pane 统计、本机/远端添加入口和项目移除操作；
 *   空态打开本机/远端应用内目录选择器；
 *   点击项目后选择项目并跳转 `/workbench`，保持 deep link 语义。
 *   来源选择与远端项目选择统一走共享 Dialog（portal / focus trap / Escape / backdrop）。
 */

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Button, Dialog, HintStatusDot } from '@/components/primitives';
import { DevicesIcon, FolderIcon, PlusIcon, PowerIcon, SyncIcon, WindowIcon, XIcon } from '@/lib/icons';
import { WorkbenchFreshRestartDialog } from '@/components/domain/WorkbenchFreshRestartDialog';
import { useWorkbenchFreshRestart } from '@/hooks/useWorkbenchFreshRestart';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
import { useLanAgentFleet } from '@/hooks/useLanAgentFleet';
import { useOptionalWorkbenchAgentHints } from '@/hooks/workbenchAgentHintsContext';
import { EMPTY_HINT_COUNTS } from '@/lib/workbenchAgentHints';
import { agentHintAriaSpec } from '@/pages/Workbench/workbenchAgentHintPresentation';
import { EMPTY_PROJECT_SESSION_STATS } from '@/lib/workbenchProjectStats';
import { fleetExceptionCount } from '@/lib/types/lanFleet';
import type { LanFleetDeviceSummary, LanFleetProjectSummary } from '@/lib/types/lanFleet';
import { WorkbenchRemoteProjectPicker } from '@/components/domain/WorkbenchRemoteProjectPicker';
import { moveProjectId } from '@/lib/workbenchRemoteProjects';
import {
  DEVICE_FILTER_ALL,
  collectDeviceFilterOptions,
  readStoredDeviceFilterId,
  resolveDeviceFilterId,
  writeStoredDeviceFilterId,
} from '@/lib/workbenchProjectDeviceFilter';
import {
  expandGroupOrderToProjectIds,
  filterProjectGroupsByDevice,
  groupWorkbenchProjects,
  otherDeviceNames,
  pickDisplayMember,
} from '@/lib/workbenchProjectGroups';
import type { WorkbenchProject } from '@/lib/types';
import styles from './WorkbenchProjectRail.module.css';

/**
 * Business Logic（为什么需要这个组件）:
 *   用户应能从任意页面选择本机或局域网远端项目文件夹进入 Workbench。
 *
 * Code Logic（这个组件做什么）:
 *   使用共享 Workbench 项目上下文渲染项目列表、terminal window/pane 统计和添加来源选择，并用 React Router 导航到 `/workbench`。
 */
export function WorkbenchProjectRail() {
  const { t } = useTranslation(['workbench', 'common']);
  const navigate = useNavigate();
  const addProjectButtonRef = useRef<HTMLButtonElement>(null);
  const [sourcePickerOpen, setSourcePickerOpen] = useState<boolean>(false);
  const [remotePickerOpen, setRemotePickerOpen] = useState<boolean>(false);
  const [remoteOpenBusy, setRemoteOpenBusy] = useState<boolean>(false);
  const [localPickerOpen, setLocalPickerOpen] = useState<boolean>(false);
  const [localOpenBusy, setLocalOpenBusy] = useState<boolean>(false);
  const [draggingProjectId, setDraggingProjectId] = useState<string | null>(null);
  const [dropIndicator, setDropIndicator] = useState<{
    projectId: string;
    position: 'before' | 'after';
  } | null>(null);
  const [previewOrderIds, setPreviewOrderIds] = useState<string[] | null>(null);
  const [removeGroup, setRemoveGroup] = useState<{
    members: WorkbenchProject[];
    selectedIds: string[];
  } | null>(null);
  const [removing, setRemoving] = useState<boolean>(false);
  const [deviceFilterId, setDeviceFilterId] = useState<string>(() => {
    return resolveDeviceFilterId(readStoredDeviceFilterId(), []);
  });
  const listRef = useRef<HTMLDivElement | null>(null);
  const draggingProjectIdRef = useRef<string | null>(null);
  const previewOrderIdsRef = useRef<string[] | null>(null);
  const pointerIdRef = useRef<number | null>(null);
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
    addProjectFromPath,
    openRemoteProject,
    selectProject,
    removeProject,
    reorderProjects,
    currentWindowLabel,
    occupancy,
    openProjectInNewWindow,
  } = useWorkbenchProjects();
  const occupancyByProject = useMemo(() => {
    const map = new Map<string, string>();
    for (const row of occupancy ?? []) map.set(row.projectId, row.windowLabel);
    return map;
  }, [occupancy]);

  const { projectSummaries, snapshot: fleetSnapshot } = useLanAgentFleet({ enabled: true });
  // ---- 设备级「全新启动连接」悬停入口：共享弹窗状态机；完成后刷新项目统计 ----
  const {
    dialog: freshRestartDialog,
    openFreshRestartDialog,
    closeFreshRestartDialog,
    confirmFreshRestart,
  } = useWorkbenchFreshRestart({ onCompleted: () => void loadProjects() });
  const agentHints = useOptionalWorkbenchAgentHints();
  const hintsForProject = agentHints?.hintsForProject;

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

  /**
   * Business Logic（为什么需要这个列表）:
   *   设备筛选下拉只展示当前项目列表中出现过的设备。
   *
   * Code Logic（这个函数做什么）:
   *   聚合 projects 的 deviceId/deviceName，本机优先。
   */
  const deviceFilterOptions = useMemo(
    () => collectDeviceFilterOptions(projects),
    [projects],
  );

  /**
   * Business Logic（为什么需要这个解析）:
   *   持久化偏好可能指向已删除设备；UI 与过滤必须用安全回退后的 id。
   *
   * Code Logic（这个函数做什么）:
   *   resolveDeviceFilterId(stored preference, live options)。
   */
  const resolvedDeviceFilterId = useMemo(
    () => resolveDeviceFilterId(deviceFilterId, deviceFilterOptions),
    [deviceFilterId, deviceFilterOptions],
  );

  const showDeviceFilter = deviceFilterOptions.length >= 2;

  /**
   * Business Logic（为什么需要这个回调）:
   *   用户切换设备筛选后应立即收窄列表并记住偏好。
   *
   * Code Logic（这个函数做什么）:
   *   更新 state + localStorage；切换时清拖拽预览，避免跨筛选脏序。
   */
  const handleDeviceFilterChange = useCallback((next: string) => {
    const value = next.trim() || DEVICE_FILTER_ALL;
    setDeviceFilterId(value);
    writeStoredDeviceFilterId(value);
    draggingProjectIdRef.current = null;
    previewOrderIdsRef.current = null;
    pointerIdRef.current = null;
    setDraggingProjectId(null);
    setDropIndicator(null);
    setPreviewOrderIds(null);
  }, []);

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
   *   关闭来源弹层后打开本机应用内选择器。
   */
  const handleAddLocalProject = useCallback(() => {
    setSourcePickerOpen(false);
    setLocalOpenBusy(false);
    setLocalPickerOpen(true);
  }, []);

  const closeLocalPicker = useCallback((options?: { force?: boolean }) => {
    if (localOpenBusy && !options?.force) return;
    setLocalPickerOpen(false);
    setLocalOpenBusy(false);
    window.setTimeout(() => addProjectButtonRef.current?.focus(), 0);
  }, [localOpenBusy]);

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
   * Business Logic（为什么需要这个列表）:
   *   侧栏展示与拖拽目标只针对当前设备筛选后的项目；active 被滤掉时工作区仍保持打开。
   *
   * Code Logic（这个函数做什么）:
   *   先按 resolvedDeviceFilterId 过滤，再叠加热拖拽 preview 序。
   */
  const allGroups = useMemo(() => groupWorkbenchProjects(projects), [projects]);

  const filteredGroups = useMemo(
    () => filterProjectGroupsByDevice(allGroups, resolvedDeviceFilterId),
    [allGroups, resolvedDeviceFilterId],
  );

  const displayGroups = useMemo(() => {
    if (!previewOrderIds) return filteredGroups;
    const byKey = new Map(filteredGroups.map((group) => [group.key, group]));
    return previewOrderIds
      .map((key) => byKey.get(key))
      .filter((group): group is (typeof filteredGroups)[number] => Boolean(group));
  }, [filteredGroups, previewOrderIds]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   列表顺序变化时用 FLIP 补间，让项目卡片滑动到新位置。
   *
   * Code Logic（这个函数做什么）:
   *   记录上帧 rect，下一帧用 Web Animations 从旧位置过渡到新位置。
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
  }, [displayGroups]);

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
    pointerIdRef.current = null;
    setDraggingProjectId(null);
    setDropIndicator(null);
    setPreviewOrderIds(null);
  }, []);

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

  const applyPointerReorder = useCallback(
    (clientY: number) => {
      const sourceId = draggingProjectIdRef.current;
      const listEl = listRef.current;
      if (!sourceId || !listEl) return;
      const target = resolveDropTarget(clientY, listEl, sourceId);
      if (!target) return;
      setDropIndicator(target);
      const base =
        previewOrderIdsRef.current ?? filteredGroups.map((group) => group.key);
      const next = moveProjectId(base, sourceId, target.projectId, target.position);
      if (next.join('\0') === base.join('\0')) return;
      previewOrderIdsRef.current = next;
      setPreviewOrderIds(next);
    },
    [filteredGroups, resolveDropTarget],
  );

  const finishPointerReorder = useCallback(() => {
    const sourceId = draggingProjectIdRef.current;
    if (!sourceId) return;
    const visibleNext =
      previewOrderIdsRef.current ?? filteredGroups.map((group) => group.key);
    const unchanged =
      visibleNext.length === filteredGroups.length &&
      visibleNext.every((key, index) => filteredGroups[index]?.key === key);
    clearDragUi();
    if (unchanged) return;
    const membersByKey: Record<string, string[]> = {};
    for (const group of allGroups) {
      membersByKey[group.key] = group.members.map((member) => member.id);
    }
    const nextFull = expandGroupOrderToProjectIds({
      fullProjectIds: projects.map((project) => project.id),
      fullGroupKeys: allGroups.map((group) => group.key),
      membersByKey,
      visibleGroupKeysNewOrder: visibleNext,
    });
    void reorderProjects(nextFull);
  }, [allGroups, clearDragUi, filteredGroups, projects, reorderProjects]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   Tauri WebView 上 HTML5 DnD 的 drop 经常不触发；改用 pointer 拖动手柄，行为更稳。
   *
   * Code Logic（这个函数做什么）:
   *   pointerdown 在手柄开始；window pointermove 预览重排；pointerup/cancel 持久化。
   */
  const handleHandlePointerDown = useCallback(
    (event: { button: number; preventDefault: () => void; stopPropagation: () => void; pointerId: number; clientY: number; currentTarget: HTMLSpanElement }, projectId: string) => {
      if (projectBusy || event.button !== 0) return;
      event.preventDefault();
      event.stopPropagation();
      const initialOrder = filteredGroups.map((group) => group.key);
      pointerIdRef.current = event.pointerId;
      draggingProjectIdRef.current = projectId;
      previewOrderIdsRef.current = initialOrder;
      setDraggingProjectId(projectId);
      setPreviewOrderIds(initialOrder);
      setDropIndicator(null);
      try {
        event.currentTarget.setPointerCapture(event.pointerId);
      } catch {
        // capture 失败时仍可用 window 级 move/up。
      }
      applyPointerReorder(event.clientY);
    },
    [applyPointerReorder, filteredGroups, projectBusy],
  );

  useEffect(() => {
    if (!draggingProjectId) return;

    const onMove = (event: PointerEvent) => {
      if (
        pointerIdRef.current != null &&
        event.pointerId !== pointerIdRef.current
      ) {
        return;
      }
      event.preventDefault();
      applyPointerReorder(event.clientY);
    };

    const onUp = (event: PointerEvent) => {
      if (
        pointerIdRef.current != null &&
        event.pointerId !== pointerIdRef.current
      ) {
        return;
      }
      finishPointerReorder();
    };

    window.addEventListener('pointermove', onMove, { passive: false });
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onUp);
    return () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
    };
  }, [applyPointerReorder, draggingProjectId, finishPointerReorder]);

  return (
    <section className={styles.rail} aria-label={sectionTitle}>
      <div className={styles.header}>
        <h2 className={styles.title}>{sectionTitle}</h2>
        <div className={styles.toolbar}>
          {showDeviceFilter ? (
            <select
              className={styles.deviceFilter}
              value={resolvedDeviceFilterId}
              aria-label={t('workbench:projectRail.deviceFilterLabel')}
              onChange={(event) => handleDeviceFilterChange(event.target.value)}
            >
              <option value={DEVICE_FILTER_ALL}>
                {t('workbench:projectRail.deviceFilterAll')}
              </option>
              {deviceFilterOptions.map((option) => (
                <option key={option.deviceId} value={option.deviceId}>
                  {option.deviceName}
                </option>
              ))}
            </select>
          ) : (
            <span className={styles.toolbarSpacer} aria-hidden="true" />
          )}
          <div className={styles.actions}>
            <Button
              variant="icon"
              icon={<SyncIcon />}
              title={t('workbench:refresh')}
              aria-label={t('workbench:refresh')}
              onClick={() => void loadProjects({ refreshIdentities: true })}
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
      </div>

      {projectError ? <div className={styles.errorBox}>{projectError}</div> : null}

      <div
        ref={listRef}
        className={styles.projectList}
        data-dragging={draggingProjectId ? true : undefined}
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
        {!projectsLoading &&
        projects.length > 0 &&
        filteredGroups.length === 0 &&
        resolvedDeviceFilterId !== DEVICE_FILTER_ALL ? (
          <div className={styles.filterEmpty}>
            <span>{t('workbench:projectRail.deviceFilterEmpty')}</span>
            <button
              type="button"
              className={styles.filterEmptyCta}
              onClick={() => handleDeviceFilterChange(DEVICE_FILTER_ALL)}
            >
              {t('workbench:projectRail.deviceFilterShowAll')}
            </button>
          </div>
        ) : null}
        {displayGroups.map((group) => {
          const project = pickDisplayMember(group.members, {
            deviceFilterId: resolvedDeviceFilterId,
          });
          const stats = projectSessionStats[project.id] ?? EMPTY_PROJECT_SESSION_STATS;
          const windowCountLabel = t('workbench:projectWindowCount', {
            count: stats.windowCount,
          });
          const paneCountLabel = t('workbench:projectPaneCount', {
            count: stats.paneCount,
          });
          const isActive = group.members.some((member) => member.id === activeProjectId);
          const occupiedElsewhere = group.members.some((member) => {
            const occupiedLabel = occupancyByProject.get(member.id);
            return Boolean(occupiedLabel && occupiedLabel !== currentWindowLabel);
          });
          const statusLabel = occupiedElsewhere
            ? t('workbench:projectRail.statusOccupied')
            : null;
          const extraDevices = otherDeviceNames(group.members, project);
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
          const hint = hintsForProject?.(project.id) ?? EMPTY_HINT_COUNTS;
          const hintAria = agentHintAriaSpec(hint);
          return (
            <div
              key={group.key}
              ref={(node) => captureItemNode(group.key, node)}
              className={styles.projectItem}
              data-project-id={group.key}
              data-active={isActive || undefined}
              data-dragging={draggingProjectId === group.key || undefined}
              data-drop-before={
                dropIndicator?.projectId === group.key && dropIndicator.position === 'before'
                  ? true
                  : undefined
              }
              data-drop-after={
                dropIndicator?.projectId === group.key && dropIndicator.position === 'after'
                  ? true
                  : undefined
              }
            >
              <span
                className={styles.dragHandle}
                role="button"
                tabIndex={projectBusy ? -1 : 0}
                title={t('workbench:projectRail.dragHandleAria')}
                aria-label={t('workbench:projectRail.dragHandleAria')}
                onPointerDown={(event) => handleHandlePointerDown(event, group.key)}
              >
                ⋮⋮
              </span>
              <button
                type="button"
                className={styles.projectSelectButton}
                title={
                  occupiedElsewhere
                    ? t('workbench:projectRail.statusOccupied')
                    : agentHint || undefined
                }
                onClick={() => {
                  void selectProject(project).then(() => {
                    if (!occupiedElsewhere) navigate('/workbench');
                  });
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
                  {extraDevices.length > 0 ? (
                    <span className={styles.projectOtherDevices}>
                      {t('workbench:projectRail.otherDevices', {
                        names: extraDevices.join(' · '),
                      })}
                    </span>
                  ) : null}
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
                  {statusLabel ? (
                    <span className={styles.projectStatusText}>{statusLabel}</span>
                  ) : null}
                </span>
              </button>
              <HintStatusDot
                className={styles.projectStatusDot}
                data-active={isActive || undefined}
                waitingCount={hint.waitingCount}
                stoppedCount={hint.stoppedCount}
                aria-label={t(hintAria.key, hintAria.values)}
              />
              <Button
                className={styles.projectOpenWindowButton}
                variant="icon"
                icon={<WindowIcon />}
                title={t('workbench:projectRail.openInNewWindow')}
                aria-label={t('workbench:projectRail.openInNewWindow')}
                data-testid="project-open-new-window"
                onClick={() => void openProjectInNewWindow(project)}
              />
              <Button
                className={styles.projectFreshRestartButton}
                variant="icon"
                icon={<PowerIcon />}
                title={t('workbench:freshStart.iconLabel')}
                aria-label={t('workbench:freshStart.iconLabel')}
                data-testid="project-fresh-restart"
                disabled={offline}
                onClick={() =>
                  openFreshRestartDialog({
                    deviceId: project.kind === 'remote' ? project.deviceId : undefined,
                  })
                }
              />
              <Button
                className={styles.projectRemoveButton}
                variant="icon"
                icon={<XIcon />}
                title={t('workbench:removeProject')}
                aria-label={t('workbench:removeProject')}
                onClick={() => {
                  if (group.members.length === 1) {
                    void removeProject(project.id);
                    return;
                  }
                  setRemoveGroup({
                    members: group.members,
                    selectedIds: group.members.map((member) => member.id),
                  });
                }}
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

      <Dialog
        open={localPickerOpen}
        titleId="workbench-local-picker-title"
        onClose={() => {
          closeLocalPicker();
        }}
        closeOnEscape={!localOpenBusy}
        closeOnBackdrop={!localOpenBusy}
        className={styles.modalDialog}
      >
        <h2 id="workbench-local-picker-title" className="sr-only">
          {t('workbench:remoteProjectPicker.localTitle')}
        </h2>
        <WorkbenchRemoteProjectPicker
          source="local"
          openLocalProject={addProjectFromPath}
          onCancel={closeLocalPicker}
          onOpenBusyChange={setLocalOpenBusy}
          onProjectOpened={() => {
            closeLocalPicker({ force: true });
            navigate('/workbench');
          }}
        />
      </Dialog>

      <WorkbenchFreshRestartDialog {...freshRestartDialog} onClose={closeFreshRestartDialog}
        onConfirm={(includeForeign) => { void confirmFreshRestart(includeForeign); }}
      />

      <Dialog
        open={removeGroup !== null}
        titleId="workbench-remove-group-title"
        onClose={() => {
          if (!removing) setRemoveGroup(null);
        }}
        closeOnEscape={!removing}
        closeOnBackdrop={!removing}
      >
        <h2 id="workbench-remove-group-title" className={styles.removeDialogTitle}>
          {t('workbench:projectRail.removeGroupTitle')}
        </h2>
        <p className={styles.removeDialogBody}>
          {t('workbench:projectRail.removeGroupBody')}
        </p>
        <div className={styles.removeGroupList} role="group" aria-labelledby="workbench-remove-group-title">
          {removeGroup?.members.map((member) => {
            const checked = removeGroup.selectedIds.includes(member.id);
            return (
              <label key={member.id} className={styles.removeGroupOption}>
                <input
                  type="checkbox"
                  checked={checked}
                  disabled={removing}
                  onChange={() => {
                    setRemoveGroup((current) => {
                      if (!current) return current;
                      const selectedIds = current.selectedIds.includes(member.id)
                        ? current.selectedIds.filter((id) => id !== member.id)
                        : [...current.selectedIds, member.id];
                      return { ...current, selectedIds };
                    });
                  }}
                />
                <span>
                  <strong>{member.deviceName}</strong>
                  <span>{member.path}</span>
                </span>
              </label>
            );
          })}
        </div>
        <div className={styles.removeDialogActions}>
          <Button
            variant="ghost"
            size="sm"
            disabled={removing}
            onClick={() => setRemoveGroup(null)}
          >
            {t('common:action.cancel')}
          </Button>
          <Button
            variant="danger"
            size="sm"
            loading={removing}
            disabled={!removeGroup || removeGroup.selectedIds.length === 0}
            onClick={() => {
              if (!removeGroup) return;
              const ids = [...removeGroup.selectedIds];
              setRemoving(true);
              void (async () => {
                try {
                  for (const id of ids) {
                    await removeProject(id);
                  }
                  setRemoveGroup(null);
                } finally {
                  setRemoving(false);
                }
              })();
            }}
          >
            {t('workbench:projectRail.removeSelected')}
          </Button>
        </div>
      </Dialog>
    </section>
  );
}
