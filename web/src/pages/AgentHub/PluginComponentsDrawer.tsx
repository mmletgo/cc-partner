/**
 * PluginComponentsDrawer — Plugin package 组件矩阵与删除预览。
 *
 * Business Logic（为什么需要这个组件）:
 *   mixed package 必须按 component 展示固定 revision、ownership、target matrix 与 residual；
 *   delete preview 区分 tombstone vs preserve，禁止把 partial 压成 green synced。
 *
 * Code Logic（这个组件做什么）:
 *   pure props 视图：复用 Drawer/Card/Pill/Button/StatusMessage；不 import @/api/*。
 */

import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Drawer, Pill, StatusMessage } from '@/components/primitives';
import type { PluginPackageReport } from '@/lib/types/agentHub';
import {
  groupDeletePreview,
  isPluginFullySynced,
  listPluginPartialBlockers,
  orderedComponentTargets,
  pluginAggregateTone,
  pluginComponentStatusTone,
} from './pluginPackagePresentation';
import styles from './AgentHub.module.css';

export interface PluginComponentsDrawerProps {
  open: boolean;
  report: PluginPackageReport | null;
  busy?: boolean;
  error?: string | null;
  onClose: () => void;
  onConfirmDelete?: () => void;
  showDeletePreview?: boolean;
}

/**
 * 渲染 Plugin package 组件 Drawer。
 *
 * Business Logic: 每 component 独立 target matrix；aggregate partial 点名 blockers。
 * Code Logic: pure view + useMemo 分组。
 */
export function PluginComponentsDrawer({
  open,
  report,
  busy = false,
  error = null,
  onClose,
  onConfirmDelete,
  showDeletePreview = true,
}: PluginComponentsDrawerProps) {
  const { t } = useTranslation(['agentHub', 'common']);

  const blockers = useMemo(
    () => (report ? listPluginPartialBlockers(report) : []),
    [report],
  );
  const deleteGroups = useMemo(
    () => groupDeletePreview(report?.deletePreview ?? null),
    [report],
  );

  return (
    <Drawer
      open={open}
      titleId="agent-hub-plugin-components-title"
      onClose={onClose}
      side="right"
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
      className={styles.drawerSurface}
    >
      <div className={styles.drawerBody} data-testid="plugin-components-drawer">
        <header className={styles.drawerHeader}>
          <h2 id="agent-hub-plugin-components-title" className={styles.sectionTitle}>
            {t('agentHub:plugin.drawerTitle')}
          </h2>
          <Button
            variant="ghost"
            size="sm"
            onClick={onClose}
            disabled={busy}
            data-testid="plugin-components-close"
          >
            {t('common:action.cancel')}
          </Button>
        </header>

        {error ? (
          <StatusMessage tone="danger" data-testid="plugin-components-error">
            {error}
          </StatusMessage>
        ) : null}

        {!report ? (
          <StatusMessage tone="info" data-testid="plugin-components-empty">
            {t('agentHub:plugin.noReport')}
          </StatusMessage>
        ) : (
          <>
            <Card variant="flat" padding="md" data-testid="plugin-package-summary">
              <div className={styles.statusPills}>
                <span className={styles.subMeta}>{report.packageDisplayName}</span>
                <Pill tone="neutral">{t(`agentHub:targets.${report.sourceTarget}`)}</Pill>
                <Pill
                  tone={pluginAggregateTone(report.aggregateStatus)}
                  data-testid="plugin-package-aggregate"
                  data-aggregate={report.aggregateStatus}
                >
                  {t(`agentHub:aggregate.${report.aggregateStatus}`)}
                </Pill>
                <Pill tone="neutral" data-testid="plugin-package-activation">
                  {t('agentHub:plugin.activationState', { state: report.activationState })}
                </Pill>
              </div>
              {!isPluginFullySynced(report.aggregateStatus) ? (
                <StatusMessage
                  tone="warn"
                  data-testid="plugin-package-not-synced"
                  className={styles.drawerSection}
                >
                  {t('agentHub:plugin.mixedStatusWarning')}
                </StatusMessage>
              ) : null}
              {report.aggregateStatus === 'partial' && blockers.length > 0 ? (
                <ul
                  className={styles.partialList}
                  data-testid="plugin-package-partial-blockers"
                >
                  {blockers.map((blocker) => (
                    <li key={blocker}>{blocker}</li>
                  ))}
                </ul>
              ) : null}
              {report.diagnostics.length > 0 ? (
                <ul className={styles.partialList} data-testid="plugin-package-diagnostics">
                  {report.diagnostics.map((d) => (
                    <li key={d}>{d}</li>
                  ))}
                </ul>
              ) : null}
            </Card>

            <section
              className={styles.drawerSection}
              aria-label={t('agentHub:plugin.componentsAria')}
            >
              {report.components.map((component) => (
                <Card
                  key={component.assetId}
                  variant="outlined"
                  padding="md"
                  className={styles.drawerSection}
                  data-testid={`plugin-component-${component.assetId}`}
                  data-revision={component.canonicalRevisionId}
                  data-ownership={component.ownership}
                >
                  <div className={styles.statusPills}>
                    <strong>{component.displayName}</strong>
                    <Pill tone="neutral">
                      {t(`agentHub:kinds.${component.kind}`, {
                        defaultValue: component.kind,
                      })}
                    </Pill>
                    <Pill tone="neutral" data-testid={`plugin-component-rev-${component.assetId}`}>
                      {component.canonicalRevisionId}
                    </Pill>
                    <Pill
                      tone="neutral"
                      data-testid={`plugin-component-ownership-${component.assetId}`}
                    >
                      {t(`agentHub:plugin.ownership.${component.ownership}`)}
                    </Pill>
                    <Pill tone="neutral">
                      {t(`agentHub:targets.${component.sourceTarget}`)}
                    </Pill>
                  </div>
                  {component.residualReason ? (
                    <p
                      className={styles.subMeta}
                      data-testid={`plugin-component-residual-${component.assetId}`}
                    >
                      {t('agentHub:plugin.residualReason', {
                        reason: component.residualReason,
                      })}
                    </p>
                  ) : null}
                  <div
                    className={styles.targets}
                    data-testid={`plugin-component-targets-${component.assetId}`}
                  >
                    {orderedComponentTargets(component).map(({ target, cell }) => (
                      <div
                        key={target}
                        className={styles.probeCard}
                        data-testid={`plugin-component-cell-${component.assetId}-${target}`}
                        data-status={cell?.status ?? 'missing'}
                      >
                        <div className={styles.statusPills}>
                          <span>{t(`agentHub:targets.${target}`)}</span>
                          {cell ? (
                            <Pill tone={pluginComponentStatusTone(cell.status)}>
                              {t(`agentHub:plugin.componentStatus.${cell.status}`)}
                            </Pill>
                          ) : (
                            <Pill tone="neutral">{t('agentHub:plugin.componentStatus.missing')}</Pill>
                          )}
                        </div>
                        {cell?.materializedAlias ? (
                          <p className={styles.subMeta}>
                            {t('agentHub:matrix.invocation')}: {cell.materializedAlias}
                          </p>
                        ) : null}
                        {cell && cell.reasons.length > 0 ? (
                          <ul className={styles.partialList}>
                            {cell.reasons.map((r) => (
                              <li key={r}>{r}</li>
                            ))}
                          </ul>
                        ) : null}
                      </div>
                    ))}
                  </div>
                </Card>
              ))}
            </section>

            {report.residuals.length > 0 ? (
              <section
                className={styles.drawerSection}
                aria-label={t('agentHub:plugin.residualsAria')}
                data-testid="plugin-residuals"
              >
                <h3 className={styles.sectionTitle}>{t('agentHub:plugin.residualsTitle')}</h3>
                <ul className={styles.partialList}>
                  {report.residuals.map((residual) => (
                    <li
                      key={`${residual.residualTarget}-${residual.residualKind}-${residual.treeManifestHash}`}
                      data-included={residual.included ? 'true' : 'false'}
                    >
                      {t(`agentHub:targets.${residual.residualTarget}`)} ·{' '}
                      {t(`agentHub:plugin.residualKind.${residual.residualKind}`)} ·{' '}
                      {residual.included
                        ? t('agentHub:plugin.residualIncluded')
                        : t('agentHub:plugin.residualOmitted')}
                      {residual.reasons.length > 0
                        ? ` (${residual.reasons.join(', ')})`
                        : ''}
                    </li>
                  ))}
                </ul>
              </section>
            ) : null}

            {showDeletePreview && report.deletePreview ? (
              <section
                className={styles.drawerSection}
                data-testid="plugin-delete-preview"
                aria-label={t('agentHub:plugin.deletePreviewAria')}
              >
                <h3 className={styles.sectionTitle}>{t('agentHub:plugin.deletePreviewTitle')}</h3>
                <StatusMessage tone="warn">{t('agentHub:plugin.deletePreviewHint')}</StatusMessage>
                <div className={styles.drawerSection}>
                  <h4 className={styles.sectionTitle}>
                    {t('agentHub:plugin.tombstoneGroup')}
                  </h4>
                  <ul className={styles.partialList} data-testid="plugin-delete-tombstone">
                    {deleteGroups.tombstone.length === 0 ? (
                      <li>{t('agentHub:plugin.deleteNone')}</li>
                    ) : (
                      deleteGroups.tombstone.map((row) => (
                        <li key={row.assetId} data-decision={row.decision}>
                          {row.displayName} · {t(`agentHub:plugin.ownership.${row.ownership}`)}
                        </li>
                      ))
                    )}
                  </ul>
                </div>
                <div className={styles.drawerSection}>
                  <h4 className={styles.sectionTitle}>
                    {t('agentHub:plugin.preserveGroup')}
                  </h4>
                  <ul className={styles.partialList} data-testid="plugin-delete-preserve">
                    {deleteGroups.preserve.length === 0 ? (
                      <li>{t('agentHub:plugin.deleteNone')}</li>
                    ) : (
                      deleteGroups.preserve.map((row) => (
                        <li key={row.assetId} data-decision={row.decision}>
                          {row.displayName} · {t(`agentHub:plugin.decision.${row.decision}`)}
                        </li>
                      ))
                    )}
                  </ul>
                </div>
                {onConfirmDelete ? (
                  <Button
                    variant="danger"
                    size="sm"
                    disabled={busy}
                    onClick={onConfirmDelete}
                    data-testid="plugin-delete-confirm"
                  >
                    {t('agentHub:plugin.confirmDelete')}
                  </Button>
                ) : null}
              </section>
            ) : null}
          </>
        )}
      </div>
    </Drawer>
  );
}

PluginComponentsDrawer.displayName = 'PluginComponentsDrawer';
