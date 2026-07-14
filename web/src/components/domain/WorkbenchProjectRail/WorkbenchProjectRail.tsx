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
 */

import { useCallback, useEffect, useId, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/primitives';
import { DevicesIcon, FolderIcon, PlusIcon, SyncIcon, XIcon } from '@/lib/icons';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
import { EMPTY_PROJECT_SESSION_STATS } from '@/lib/workbenchProjectStats';
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
  const sourcePopoverRef = useRef<HTMLDivElement>(null);
  const remoteDialogRef = useRef<HTMLDivElement>(null);
  const sourcePopoverId = useId();
  const remoteDialogId = useId();
  const [sourcePickerOpen, setSourcePickerOpen] = useState<boolean>(false);
  const [remotePickerOpen, setRemotePickerOpen] = useState<boolean>(false);
  const [remoteOpenBusy, setRemoteOpenBusy] = useState<boolean>(false);
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
  } = useWorkbenchProjects();

  const sectionTitle = t('workbench:projectRail.sectionTitle');

  /**
   * Business Logic（为什么需要这个函数）:
   *   关闭远端选择器后应把焦点还给“添加项目”按钮，避免键盘焦点丢失。
   *
   * Code Logic（这个函数做什么）:
   *   busy 且非 force 时忽略；否则关闭弹层并异步 focus 添加按钮。
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

  useEffect(() => {
    if (!sourcePickerOpen) return;

    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (
        sourcePopoverRef.current?.contains(target) ||
        addProjectButtonRef.current?.contains(target)
      ) {
        return;
      }
      setSourcePickerOpen(false);
    };

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        setSourcePickerOpen(false);
        addProjectButtonRef.current?.focus();
      }
    };

    document.addEventListener('pointerdown', handlePointerDown, true);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('pointerdown', handlePointerDown, true);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [sourcePickerOpen]);

  useEffect(() => {
    if (!remotePickerOpen) return;

    const focusTimer = window.setTimeout(() => remoteDialogRef.current?.focus(), 0);
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') closeRemotePicker();
    };

    document.addEventListener('keydown', handleKeyDown);
    return () => {
      window.clearTimeout(focusTimer);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [closeRemotePicker, remotePickerOpen]);

  return (
    <section className={styles.rail} aria-label={sectionTitle}>
      <div className={styles.header}>
        <h2 className={styles.title}>{sectionTitle}</h2>
        <div className={styles.actions}>
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
            aria-controls={
              sourcePickerOpen ? sourcePopoverId : remotePickerOpen ? remoteDialogId : undefined
            }
            loading={projectBusy}
            onClick={() => setSourcePickerOpen((open) => !open)}
          />
          {sourcePickerOpen ? (
            <div
              ref={sourcePopoverRef}
              id={sourcePopoverId}
              className={styles.sourcePopover}
              role="dialog"
              aria-label={t('workbench:addProject')}
            >
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
            </div>
          ) : null}
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
          return (
            <div
              key={project.id}
              className={styles.projectItem}
              data-active={isActive || undefined}
            >
              <button
                type="button"
                className={styles.projectSelectButton}
                onClick={() => {
                  void selectProject(project).then(() => navigate('/workbench'));
                }}
              >
                <span className={styles.projectText}>
                  <span className={styles.projectName}>{project.name}</span>
                  <span className={styles.projectPath}>{project.path}</span>
                  <span className={styles.projectMeta}>
                    <span className={styles.projectDevice}>
                      {project.kind === 'remote' ? (
                        <span className={styles.remoteBadge}>{t('workbench:remoteBadge')}</span>
                      ) : null}
                      <span>{project.deviceName}</span>
                    </span>
                    <span
                      className={styles.projectStats}
                      aria-label={`${windowCountLabel}, ${paneCountLabel}`}
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

      {remotePickerOpen ? (
        <div className={styles.modalBackdrop} role="presentation">
          <div
            id={remoteDialogId}
            ref={remoteDialogRef}
            className={styles.modalDialog}
            role="dialog"
            aria-modal="true"
            aria-label={t('workbench:remoteProjectPicker.title')}
            tabIndex={-1}
          >
            <WorkbenchRemoteProjectPicker
              openProject={openRemoteProject}
              onCancel={closeRemotePicker}
              onOpenBusyChange={setRemoteOpenBusy}
              onProjectOpened={() => {
                closeRemotePicker({ force: true });
                navigate('/workbench');
              }}
            />
          </div>
        </div>
      ) : null}
    </section>
  );
}
