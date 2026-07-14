/**
 * WorkbenchProjectRail（侧栏项目文件夹入口）
 *
 * Business Logic（为什么需要这个组件）:
 *   项目文件夹列表是进入工作台的主要入口，不需要再占用一个独立导航菜单项。
 *
 * Code Logic（这个组件做什么）:
 *   渲染设置菜单项下方的项目列表、window/pane 统计、本机/远端添加入口和项目移除操作；点击项目后选择项目并跳转 `/workbench`。
 *   来源选择与远端项目选择统一走共享 Dialog（portal / focus trap / Escape / backdrop）。
 */

import { useCallback, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Button, Dialog } from '@/components/primitives';
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

  return (
    <section className={styles.rail} aria-label={t('workbench:projectFolders')}>
      <div className={styles.header}>
        <span className={styles.title}>{t('workbench:projectFolders')}</span>
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
            <span>{t('workbench:emptyProjects')}</span>
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
          return (
            <div
              key={project.id}
              className={styles.projectItem}
              data-active={project.id === activeProjectId || undefined}
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
                </span>
              </button>
              <span
                className={styles.projectStatusDot}
                data-active={project.id === activeProjectId || undefined}
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
          onClick={() => {
            setSourcePickerOpen(false);
            void chooseAndAddProject().then((project) => {
              if (project) navigate('/workbench');
            });
          }}
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
          onClick={() => {
            setSourcePickerOpen(false);
            setRemoteOpenBusy(false);
            setRemotePickerOpen(true);
          }}
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
