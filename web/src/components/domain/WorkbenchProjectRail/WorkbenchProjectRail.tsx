/**
 * WorkbenchProjectRail（侧栏项目文件夹入口）
 *
 * Business Logic（为什么需要这个组件）:
 *   项目文件夹列表是进入工作台的主要入口，不需要再占用一个独立导航菜单项。
 *
 * Code Logic（这个组件做什么）:
 *   渲染设置菜单项下方的项目列表、添加项目表单和项目移除操作；点击项目后选择项目并跳转 `/workbench`。
 */

import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Button, Input } from '@/components/primitives';
import { FolderIcon, PlusIcon, SyncIcon, XIcon } from '@/lib/icons';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
import styles from './WorkbenchProjectRail.module.css';

/**
 * Business Logic（为什么需要这个组件）:
 *   用户应能从任意页面直接选择项目文件夹进入 Workbench。
 *
 * Code Logic（这个组件做什么）:
 *   使用共享 Workbench 项目上下文渲染项目列表和添加表单，并用 React Router 导航到 `/workbench`。
 */
export function WorkbenchProjectRail() {
  const { t } = useTranslation(['workbench']);
  const navigate = useNavigate();
  const {
    projects,
    activeProjectId,
    projectPath,
    projectFormOpen,
    projectsLoading,
    projectBusy,
    projectError,
    setProjectPath,
    setProjectFormOpen,
    loadProjects,
    chooseProjectDirectory,
    addProject,
    selectProject,
    removeProject,
  } = useWorkbenchProjects();

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
            variant="icon"
            icon={<PlusIcon />}
            title={
              projectFormOpen
                ? t('workbench:hideAddProject')
                : t('workbench:showAddProject')
            }
            aria-label={
              projectFormOpen
                ? t('workbench:hideAddProject')
                : t('workbench:showAddProject')
            }
            onClick={() => setProjectFormOpen((current) => !current)}
          />
        </div>
      </div>

      {projectFormOpen ? (
        <div className={styles.addForm}>
          <Input
            value={projectPath}
            onChange={(event) => setProjectPath(event.target.value)}
            placeholder={t('workbench:projectPathPlaceholder')}
            size="sm"
            mono
          />
          <div className={styles.addActions}>
            <Button size="sm" variant="secondary" onClick={() => void chooseProjectDirectory()}>
              {t('workbench:chooseFolder')}
            </Button>
            <Button
              size="sm"
              variant="primary"
              icon={<PlusIcon />}
              loading={projectBusy}
              disabled={!projectPath.trim()}
              onClick={() => {
                void addProject().then((project) => {
                  if (project) navigate('/workbench');
                });
              }}
            >
              {t('workbench:addProject')}
            </Button>
          </div>
        </div>
      ) : null}

      {projectError ? <div className={styles.errorBox}>{projectError}</div> : null}

      <div className={styles.projectList}>
        {projectsLoading ? <div className={styles.muted}>{t('workbench:loading')}</div> : null}
        {!projectsLoading && projects.length === 0 ? (
          <div className={styles.emptyProject}>
            <FolderIcon />
            <span>{t('workbench:emptyProjects')}</span>
          </div>
        ) : null}
        {projects.map((project) => (
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
                  <span>{project.deviceName}</span>
                  <span>{t('workbench:openWorkbench')}</span>
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
        ))}
      </div>
    </section>
  );
}
