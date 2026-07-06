import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import type { WorkbenchProject } from '@/lib/types';
import { canSelectMobileProject } from '../mobileWorkbenchState';
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
 *   手机进入 `/mobile` 后需要先选择最近 Workbench 项目；本机项目进入完整工作台，远端快捷方式进入自动化代理链路。
 *
 * Code Logic（这个组件做什么）:
 *   渲染最近项目列表、刷新入口、加载态、错误态和空态；local/remote 项目点击后把 DTO 交给父组件，未知类型保持可聚焦并用 aria-disabled 展示提示。
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
          const canSelect = canSelectMobileProject(project);
          const unsupportedNoticeId = `mobile-project-${project.id}-unsupported`;
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
              } ${canSelect ? '' : styles.mobileListItemDisabled}`}
              aria-pressed={isActive}
              aria-disabled={!canSelect}
              aria-describedby={canSelect ? undefined : unsupportedNoticeId}
              onClick={(event) => {
                if (!canSelect) {
                  event.preventDefault();
                  return;
                }
                onSelect(project);
              }}
              onKeyDown={(event) => {
                if (canSelect) return;
                if (event.key === 'Enter' || event.key === ' ') {
                  event.preventDefault();
                }
              }}
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
              {canSelect ? null : (
                <span id={unsupportedNoticeId} className={styles.mobileListNotice}>
                  {t('workbench:mobile.projectPanel.unsupportedProjectKind')}
                </span>
              )}
            </button>
          );
        })}
      </div>
    </section>
  );
}
