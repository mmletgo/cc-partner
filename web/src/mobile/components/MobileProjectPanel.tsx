import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import type { WorkbenchProject } from '@/lib/types';
import { MoreIcon } from '@/lib/icons';
import { useLanAgentFleet } from '@/hooks/useLanAgentFleet';
import { fleetExceptionCount } from '@/lib/types/lanFleet';
import { canSelectMobileProject } from '../mobileWorkbenchState';
import styles from '../MobileWorkbench.module.css';

export interface MobileProjectPanelProps {
  projects: WorkbenchProject[];
  activeProjectId: string | null;
  loading: boolean;
  error: string | null;
  onSelect: (project: WorkbenchProject) => void;
  onRefresh: () => void;
  onAddLocal: () => void;
  onAddRemote: () => void;
  onRemoveRequest: (project: WorkbenchProject) => void;
}

/**
 * MobileProjectPanel（移动端项目选择面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机进入 `/mobile` 后需要选择、添加或移除 Workbench 项目；本机项目进入完整工作台，远端快捷方式走二级代理。
 *
 * Code Logic（这个组件做什么）:
 *   渲染添加本机/局域网入口、最近项目列表、刷新、加载/错误/空态；行尾 ⋯ 触发移除请求，不直接删。
 */
export function MobileProjectPanel({
  projects,
  activeProjectId,
  loading,
  error,
  onSelect,
  onRefresh,
  onAddLocal,
  onAddRemote,
  onRemoveRequest,
}: MobileProjectPanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const shouldShowEmpty = !loading && !error && projects.length === 0;
  const { snapshot: fleetSnapshot, projectSummaries } = useLanAgentFleet({ enabled: true });

  let exceptionTotal = 0;
  for (const summary of Object.values(projectSummaries)) {
    exceptionTotal += fleetExceptionCount(summary.agentCounts);
  }
  const offlineDevices =
    fleetSnapshot?.devices.filter((d) => d.reachability === 'offline').length ?? 0;

  return (
    <section className={styles.panel} aria-labelledby="mobile-project-panel-title">
      <div className={styles.panelHeaderRow}>
        <div className={styles.panelHeader}>
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

      <div className={styles.projectAddRow}>
        <button type="button" className={styles.primaryButton} disabled={loading} onClick={onAddLocal}>
          {t('workbench:mobile.projectPanel.addLocal')}
        </button>
        <button
          type="button"
          className={styles.secondaryButton}
          disabled={loading}
          onClick={onAddRemote}
        >
          {t('workbench:mobile.projectPanel.addRemote')}
        </button>
      </div>

      {fleetSnapshot ? (
        <div className={styles.fleetSummary} aria-label={t('workbench:fleet.title')}>
          <p className={styles.panelState}>
            {t('workbench:fleet.title')}
            {exceptionTotal > 0
              ? ` · ${t('workbench:fleet.exceptionBadge', { count: exceptionTotal })}`
              : ''}
            {offlineDevices > 0
              ? ` · ${t('workbench:projectRail.deviceOffline')} (${offlineDevices})`
              : ''}
          </p>
        </div>
      ) : null}

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
            <div
              key={project.id}
              className={`${styles.mobileListItemRow} ${
                isActive ? styles.mobileListItemActive : ''
              } ${canSelect ? '' : styles.mobileListItemDisabled}`}
            >
              <button
                type="button"
                className={styles.mobileListItemButton}
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
              <button
                type="button"
                className={styles.moreButton}
                aria-label={t('workbench:mobile.projectPanel.moreActions')}
                onClick={() => onRemoveRequest(project)}
              >
                <MoreIcon />
              </button>
            </div>
          );
        })}
      </div>
    </section>
  );
}
