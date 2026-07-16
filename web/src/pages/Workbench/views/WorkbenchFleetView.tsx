/**
 * WorkbenchFleetView — LAN Agent Fleet 只读详情视图
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要按 owning device 查看 Agent/Attention/Git/browser/Orchestrator 摘要，
 *   且所有动作仅导航到既有 authority，禁止调度/迁移/复制/inline mutation。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 device 分组与 project 行；链接到 project / attention；cached/offline 文本+图标。
 */

import type { ReactElement } from 'react';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import type {
  FleetBrowserState,
  FleetGitState,
  FleetReachability,
  LanFleetDeviceSummary,
  LanFleetProjectSummary,
  LanFleetSnapshot,
} from '@/lib/types/lanFleet';
import { fleetExceptionCount } from '@/lib/types/lanFleet';
import styles from './WorkbenchFleetView.module.css';

export interface WorkbenchFleetViewProps {
  snapshot: LanFleetSnapshot | null;
  loading?: boolean;
  error?: string | null;
  onRefresh?: () => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   可达性文案需文本，不能只靠颜色。
 *
 * Code Logic（这个函数做什么）:
 *   映射 reachability → i18n key 后缀。
 */
function reachabilityKey(r: FleetReachability): string {
  return r;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git 摘要展示 clean/dirty/conflict/unknown。
 *
 * Code Logic（这个函数做什么）:
 *   原样返回 state token 供 i18n。
 */
function gitKey(g: FleetGitState): string {
  return g;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Browser 摘要展示 active/absent/unknown。
 *
 * Code Logic（这个函数做什么）:
 *   原样返回 state token。
 */
function browserKey(b: FleetBrowserState): string {
  return b;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   Fleet 是跨项目只读聚合入口。
 *
 * Code Logic（这个组件做什么）:
 *   列表 devices → projects；仅 Link 导航，无 mutation 按钮。
 */
export function WorkbenchFleetView({
  snapshot,
  loading = false,
  error = null,
  onRefresh,
}: WorkbenchFleetViewProps): ReactElement {
  const { t } = useTranslation(['workbench']);

  return (
    <section className={styles.root} aria-label={t('workbench:fleet.title')}>
      <header className={styles.header}>
        <div>
          <h1 className={styles.title}>{t('workbench:fleet.title')}</h1>
          <p className={styles.subtitle}>{t('workbench:fleet.subtitle')}</p>
        </div>
        <div className={styles.headerActions}>
          <Link className={styles.navLink} to="/workbench">
            {t('workbench:fleet.backToWorkbench')}
          </Link>
          {onRefresh ? (
            <button type="button" className={styles.refreshButton} onClick={onRefresh}>
              {t('workbench:refresh')}
            </button>
          ) : null}
        </div>
      </header>

      {loading && !snapshot ? (
        <p className={styles.state}>{t('workbench:loading')}</p>
      ) : null}
      {error ? (
        <p className={styles.error} role="alert">
          {error}
        </p>
      ) : null}
      {snapshot?.truncated ? (
        <p className={styles.state}>{t('workbench:fleet.truncated')}</p>
      ) : null}

      {!snapshot || snapshot.devices.length === 0 ? (
        !loading ? <p className={styles.state}>{t('workbench:fleet.empty')}</p> : null
      ) : (
        <div className={styles.deviceList}>
          {snapshot.devices.map((device) => (
            <DeviceSection key={device.deviceId} device={device} />
          ))}
        </div>
      )}
    </section>
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   按 owning device 分组展示 reachability/slots/capturedAt。
 *
 * Code Logic（这个组件做什么）:
 *   device header + project rows。
 */
function DeviceSection({ device }: { device: LanFleetDeviceSummary }): ReactElement {
  const { t } = useTranslation(['workbench']);
  const slots =
    device.schedulerSlotsUsed != null && device.schedulerSlotsMax != null
      ? t('workbench:fleet.slots', {
          used: device.schedulerSlotsUsed,
          max: device.schedulerSlotsMax,
        })
      : t('workbench:fleet.slotsUnknown');

  return (
    <section className={styles.deviceCard} aria-labelledby={`fleet-device-${device.deviceId}`}>
      <header className={styles.deviceHeader}>
        <h2 id={`fleet-device-${device.deviceId}`} className={styles.deviceName}>
          {device.deviceName || device.deviceId}
        </h2>
        <ul className={styles.deviceMeta}>
          <li>
            <span className={styles.metaLabel}>{t('workbench:fleet.reachability')}</span>
            <span data-reachability={device.reachability}>
              {t(`workbench:fleet.reachabilityStates.${reachabilityKey(device.reachability)}`)}
            </span>
          </li>
          <li>
            <span className={styles.metaLabel}>{t('workbench:fleet.freshness')}</span>
            <span data-freshness={device.freshness}>
              {t(`workbench:fleet.freshnessStates.${device.freshness}`)}
            </span>
          </li>
          <li>
            <span className={styles.metaLabel}>{t('workbench:fleet.slotsLabel')}</span>
            <span>{slots}</span>
          </li>
          {device.capturedAt ? (
            <li>
              <span className={styles.metaLabel}>{t('workbench:fleet.capturedAt')}</span>
              <time dateTime={device.capturedAt}>{device.capturedAt}</time>
            </li>
          ) : null}
          {device.errorCode ? (
            <li>
              <span className={styles.metaLabel}>{t('workbench:fleet.errorCode')}</span>
              <span>{device.errorCode}</span>
            </li>
          ) : null}
        </ul>
      </header>

      {device.projects.length === 0 ? (
        <p className={styles.state}>{t('workbench:fleet.noProjects')}</p>
      ) : (
        <ul className={styles.projectList}>
          {device.projects.map((project) => (
            <ProjectRow key={project.projectId} project={project} />
          ))}
        </ul>
      )}
    </section>
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   单 project 只读行 + 导航到权威界面。
 *
 * Code Logic（这个组件做什么）:
 *   展示 counts/git/browser/orchestrator；Link 打开项目与 Attention。
 */
/**
 * Business Logic（为什么需要这个组件）:
 *   Fleet 展示 7d Agent activity；unsupported/unavailable 不得显示 0 tokens。
 *
 * Code Logic（这个组件做什么）:
 *   按 agentActivityStatus 渲染文案或 sessions/coverage。
 */
function AgentActivityCell({ project }: { project: LanFleetProjectSummary }): ReactElement {
  const { t } = useTranslation(['workbench']);
  const status = project.agentActivityStatus ?? 'unavailable';
  if (status === 'unsupported') {
    return <>{t('workbench:fleet.agentActivityUnsupported')}</>;
  }
  if (status === 'unavailable' || !project.agentActivity) {
    return <>{t('workbench:fleet.agentActivityUnavailable')}</>;
  }
  const activity = project.agentActivity;
  const tokens =
    activity.inputTokens == null && activity.outputTokens == null
      ? t('workbench:agentLedger.unavailable')
      : t('workbench:fleet.agentActivityTokens', {
          input: activity.inputTokens ?? t('workbench:agentLedger.unavailable'),
          output: activity.outputTokens ?? t('workbench:agentLedger.unavailable'),
        });
  return (
    <>
      {t('workbench:fleet.agentActivitySummary', {
        sessions: activity.sessions,
        completed: activity.completed,
        failed: activity.failed,
        coverage: t(`workbench:agentLedger.coverage.${activity.usageCoverage}`),
        tokens,
      })}
    </>
  );
}

function ProjectRow({ project }: { project: LanFleetProjectSummary }): ReactElement {
  const { t } = useTranslation(['workbench']);
  const exceptions = fleetExceptionCount(project.agentCounts);
  const workbenchHref = `/workbench?projectId=${encodeURIComponent(project.projectId)}`;
  const attentionHref = `/attention?projectId=${encodeURIComponent(project.projectId)}`;

  return (
    <li className={styles.projectRow}>
      <div className={styles.projectMain}>
        <span className={styles.projectName}>{project.displayName}</span>
        <span className={styles.projectKind}>
          {project.projectKind === 'unavailable'
            ? t('workbench:fleet.projectUnavailable')
            : project.projectKind === 'remote'
              ? t('workbench:remoteBadge')
              : t('workbench:projectSources.local')}
        </span>
      </div>
      <dl className={styles.projectStats}>
        <div>
          <dt>{t('workbench:fleet.agents')}</dt>
          <dd>
            {t('workbench:fleet.agentCounts', {
              working: project.agentCounts.working,
              needsInput: project.agentCounts.needsInput,
              failed: project.agentCounts.failed,
              idle: project.agentCounts.idle,
            })}
            {exceptions > 0 ? (
              <span className={styles.exceptionBadge}>
                {t('workbench:fleet.exceptionBadge', { count: exceptions })}
              </span>
            ) : null}
          </dd>
        </div>
        <div>
          <dt>{t('workbench:fleet.attention')}</dt>
          <dd>{project.attentionCount}</dd>
        </div>
        <div>
          <dt>{t('workbench:fleet.git')}</dt>
          <dd>{t(`workbench:fleet.gitStates.${gitKey(project.gitState)}`)}</dd>
        </div>
        <div>
          <dt>{t('workbench:fleet.browser')}</dt>
          <dd>{t(`workbench:fleet.browserStates.${browserKey(project.browserState)}`)}</dd>
        </div>
        <div>
          <dt>{t('workbench:fleet.orchestrator')}</dt>
          <dd>
            {t('workbench:fleet.orchestratorCounts', {
              running: project.orchestratorRunning,
              retrying: project.orchestratorRetrying,
            })}
          </dd>
        </div>
        <div>
          <dt>{t('workbench:fleet.terminals')}</dt>
          <dd>{project.terminalCount}</dd>
        </div>
        <div>
          <dt>{t('workbench:fleet.agentActivity')}</dt>
          <dd data-testid={`fleet-agent-activity-${project.projectId}`}>
            <AgentActivityCell project={project} />
          </dd>
        </div>
      </dl>
      <div className={styles.projectActions}>
        <Link className={styles.navLink} to={workbenchHref}>
          {t('workbench:fleet.openProject')}
        </Link>
        {project.attentionCount > 0 ? (
          <Link className={styles.navLink} to={attentionHref}>
            {t('workbench:fleet.openAttention')}
          </Link>
        ) : null}
      </div>
    </li>
  );
}
