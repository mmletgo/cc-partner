import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import type { WorkbenchProject } from '@/lib/types';
import styles from '../MobileWorkbench.module.css';

export interface MobileProjectPanelProps {
  projects: WorkbenchProject[];
  activeProjectId: string | null;
  loading: boolean;
  error: string | null;
  onSelect: (project: WorkbenchProject) => void;
  onRefresh: () => void;
}

/**
 * MobileProjectPanel（移动端项目选择面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机进入 `/mobile` 后需要先选择最近 Workbench 项目，后续 worktree、terminal session 和状态栏都依赖该项目上下文。
 *
 * Code Logic（这个组件做什么）:
 *   渲染最近项目列表、刷新入口、加载态、错误态和空态；点击项目时把完整 WorkbenchProject DTO 交给父组件加载详情。
 */
export function MobileProjectPanel({
  projects,
  activeProjectId,
  loading,
  error,
  onSelect,
  onRefresh,
}: MobileProjectPanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const shouldShowEmpty = !loading && !error && projects.length === 0;

  return (
    <section className={styles.panel} aria-labelledby="mobile-project-panel-title">
      <div className={styles.panelHeaderRow}>
        <div className={styles.panelHeader}>
          <p className={styles.panelKicker}>{t('workbench:mobile.kicker')}</p>
          <h1 id="mobile-project-panel-title">{t('workbench:mobile.projectPanel.title')}</h1>
        </div>
        <button
          type="button"
          className={styles.secondaryButton}
          disabled={loading}
          onClick={onRefresh}
        >
          {t('workbench:refresh')}
        </button>
      </div>

      {loading ? <p className={styles.panelState}>{t('workbench:loading')}</p> : null}
      {error ? (
        <p className={styles.panelError}>
          <span>{t('workbench:mobile.projectPanel.error')}</span>
          <span>{error}</span>
        </p>
      ) : null}
      {shouldShowEmpty ? (
        <p className={styles.panelState}>{t('workbench:emptyProjects')}</p>
      ) : null}

      <div className={styles.mobileList}>
        {projects.map((project) => {
          const isActive = project.id === activeProjectId;
          const kindLabel =
            project.kind === 'remote'
              ? t('workbench:remoteBadge')
              : project.kind === 'local'
                ? t('workbench:projectSources.local')
                : project.kind;

          return (
            <button
              key={project.id}
              type="button"
              className={`${styles.mobileListItem} ${
                isActive ? styles.mobileListItemActive : ''
              }`}
              aria-pressed={isActive}
              onClick={() => onSelect(project)}
            >
              <span className={styles.mobileListTitleRow}>
                <strong className={styles.mobileListTitle}>{project.name}</strong>
                <span
                  className={`${styles.mobileBadge} ${
                    project.kind === 'remote' ? styles.mobileBadgeAccent : ''
                  }`}
                >
                  {kindLabel}
                </span>
              </span>
              <span className={styles.mobileListPath}>{project.path}</span>
              <span className={styles.mobileListMeta}>{project.deviceName}</span>
            </button>
          );
        })}
      </div>
    </section>
  );
}
