/**
 * PortableAssetDetailsDrawer — 四类 portable 资产详情 Drawer。
 *
 * Business Logic（为什么需要这个组件）:
 *   Skill/Command/Plugin/MCP 需要各自语义的详情与危险区动作；
 *   delete-everywhere 只在详情 danger zone，不用 window.confirm。
 *
 * Code Logic（这个组件做什么）:
 *   pure props 视图：复用 Drawer/Button/Card/Pill/StatusMessage；不 import @/api/*。
 */

import { useMemo, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Drawer, Pill, StatusMessage } from '@/components/primitives';
import type {
  PortableAssetActionKind,
  PortableInventoryItemDto,
} from '@/lib/types/portableInventory';
import styles from '../AgentHub.module.css';
import { CommandDetails } from './CommandDetails';
import { McpDetails } from './McpDetails';
import { SkillDetails } from './SkillDetails';

/** Plugin 详情摘要（由 PluginComponentsDrawer/presentation 投影，不持有 API）。 */
export interface PortablePluginDetailsSummary {
  packageDisplayName: string;
  activationState: string;
  aggregateStatus: string;
  componentCount: number;
  residualCount: number;
  deleteTombstoneCount: number;
  deletePreserveCount: number;
}

export interface PortableAssetDetailsDrawerProps {
  open: boolean;
  item: PortableInventoryItemDto | null;
  pluginReport?: PortablePluginDetailsSummary | null;
  busy?: boolean;
  error?: string | null;
  /** inventory stale / mutationBlocked 时不暴露 Enable/Disable/Uninstall 等 mutation。 */
  mutationBlocked?: boolean;
  stale?: boolean;
  onClose: () => void;
  onRequestAction: (action: PortableAssetActionKind) => void;
}

/**
 * Business Logic: unsupported/stale 不暴露 mutation 动作。
 * Code Logic: capabilities + managementState + inventory mutationBlocked 门闩。
 */
function mutationAllowed(
  item: PortableInventoryItemDto,
  inventoryBlocked: boolean,
): boolean {
  if (inventoryBlocked) return false;
  if (item.managementState === 'unsupported') return false;
  if (!item.projectOptedIn && item.scopeKind === 'project') return false;
  return true;
}

/**
 * Business Logic: 四类详情与 danger zone。
 * Code Logic: hooks 全在 early return 前。
 */
export function PortableAssetDetailsDrawer({
  open,
  item,
  pluginReport = null,
  busy = false,
  error = null,
  mutationBlocked = false,
  stale = false,
  onClose,
  onRequestAction,
}: PortableAssetDetailsDrawerProps) {
  const { t } = useTranslation(['agentHub', 'common']);
  const closeRef = useRef<HTMLButtonElement | null>(null);

  const inventoryBlocked = mutationBlocked || stale;
  const canMutate = useMemo(
    () => (item ? mutationAllowed(item, inventoryBlocked) : false),
    [item, inventoryBlocked],
  );
  const reasonCode = item?.capabilities.reasonCode ?? null;
  const canEnable = Boolean(item?.capabilities.canEnable && canMutate);
  const canDisable = Boolean(item?.capabilities.canDisable && canMutate);
  const canUninstall = Boolean(item?.capabilities.canUninstall && canMutate);
  const canAdopt = Boolean(item?.capabilities.canAdopt && canMutate);
  const canInstall = Boolean(item?.capabilities.canInstallToSourceTarget && canMutate);

  const skillLabels = useMemo(
    () => ({
      treeHash: t('agentHub:portable.details.treeHash'),
      origin: t('agentHub:portable.details.origin'),
      invocation: t('agentHub:portable.details.invocation'),
      sourcePath: t('agentHub:portable.details.sourcePath'),
      description: t('agentHub:portable.details.description'),
      missing: t('agentHub:portable.details.missing'),
      parentPlugin: t('agentHub:portable.details.parentPlugin'),
    }),
    [t],
  );
  const commandLabels = useMemo(
    () => ({
      nativeId: t('agentHub:portable.details.nativeId'),
      sourceFile: t('agentHub:portable.details.sourceFile'),
      invocation: t('agentHub:portable.details.invocation'),
      compatibility: t('agentHub:portable.details.compatibility'),
      missing: t('agentHub:portable.details.missing'),
      none: t('agentHub:portable.details.none'),
    }),
    [t],
  );
  const mcpLabels = useMemo(
    () => ({
      transport: t('agentHub:portable.details.transport'),
      source: t('agentHub:portable.details.source'),
      credentialPresent: t('agentHub:portable.details.credentialPresent'),
      credentialHash: t('agentHub:portable.details.credentialHash'),
      presentYes: t('agentHub:portable.details.presentYes'),
      presentNo: t('agentHub:portable.details.presentNo'),
      missing: t('agentHub:portable.details.missing'),
    }),
    [t],
  );

  return (
    <Drawer
      open={open}
      titleId="portable-asset-details-title"
      onClose={onClose}
      side="right"
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
      className={styles.drawerSurface}
    >
      <div className={styles.drawerBody} data-testid="portable-asset-details-drawer">
        <header className={styles.drawerHeader}>
          <h2 id="portable-asset-details-title" className={styles.sectionTitle}>
            {item
              ? t('agentHub:portable.details.titleWithName', { name: item.displayName })
              : t('agentHub:portable.details.title')}
          </h2>
          <Button
            ref={closeRef}
            variant="ghost"
            size="sm"
            onClick={onClose}
            disabled={busy}
            data-testid="portable-asset-details-close"
          >
            {t('common:action.cancel')}
          </Button>
        </header>

        {error ? (
          <StatusMessage tone="danger" data-testid="portable-asset-details-error">
            {error}
          </StatusMessage>
        ) : null}

        {!item ? (
          <StatusMessage tone="info" data-testid="portable-asset-details-empty">
            {t('agentHub:portable.details.empty')}
          </StatusMessage>
        ) : (
          <>
            <Card variant="flat" padding="md" data-testid="portable-asset-summary">
              <div className={styles.statusPills}>
                <strong>{item.displayName}</strong>
                <Pill tone="neutral">{t(`agentHub:kinds.${item.kind}`)}</Pill>
                <Pill tone="neutral">{t(`agentHub:targets.${item.target}`)}</Pill>
                <Pill
                  tone={item.managementState === 'unsupported' ? 'danger' : 'neutral'}
                  data-testid="portable-asset-management"
                  data-state={item.managementState}
                >
                  {t(`agentHub:portable.management.${item.managementState}`)}
                </Pill>
                <Pill tone="neutral" data-testid="portable-asset-actual-enabled">
                  {item.actualEnabled === null
                    ? t('agentHub:portable.details.actualUnknown')
                    : item.actualEnabled
                      ? t('agentHub:portable.details.actualEnabled')
                      : t('agentHub:portable.details.actualDisabled')}
                </Pill>
              </div>
              <div className={styles.metaBlock}>
                <div>
                  <span className={styles.metaLabel}>
                    {t('agentHub:portable.details.canonical')}
                  </span>
                  <span className={styles.mono}>
                    {item.canonicalRevisionId ?? t('agentHub:portable.details.missing')}
                  </span>
                </div>
                <div>
                  <span className={styles.metaLabel}>
                    {t('agentHub:portable.details.desired')}
                  </span>
                  <span data-testid="portable-asset-desired">
                    {item.desiredPresence ?? t('agentHub:portable.details.missing')}
                    {item.desiredEnabled === null
                      ? ''
                      : ` / ${
                          item.desiredEnabled
                            ? t('agentHub:portable.details.desiredEnabled')
                            : t('agentHub:portable.details.desiredDisabled')
                        }`}
                  </span>
                </div>
                <div>
                  <span className={styles.metaLabel}>
                    {t('agentHub:portable.details.materialization')}
                  </span>
                  <span>
                    {item.materializationStatus ?? t('agentHub:portable.details.missing')}
                  </span>
                </div>
              </div>
              {reasonCode || item.warnings.length > 0 ? (
                <StatusMessage
                  tone="warn"
                  data-testid="portable-asset-diagnostic"
                  className={styles.drawerSection}
                >
                  {reasonCode ?? item.warnings[0]}
                </StatusMessage>
              ) : null}
            </Card>

            {item.kind === 'skill' ? <SkillDetails item={item} labels={skillLabels} /> : null}
            {item.kind === 'command' ? (
              <CommandDetails item={item} labels={commandLabels} />
            ) : null}
            {item.kind === 'mcp' ? <McpDetails item={item} labels={mcpLabels} /> : null}
            {item.kind === 'plugin' ? (
              <section className={styles.drawerSection} data-testid="portable-plugin-details">
                <div className={styles.metaBlock}>
                  <div>
                    <span className={styles.metaLabel}>
                      {t('agentHub:portable.details.package')}
                    </span>
                    <span data-testid="portable-plugin-package">
                      {pluginReport?.packageDisplayName ?? item.displayName}
                    </span>
                  </div>
                  <div>
                    <span className={styles.metaLabel}>
                      {t('agentHub:portable.details.activation')}
                    </span>
                    <span data-testid="portable-plugin-activation">
                      {pluginReport?.activationState ?? t('agentHub:portable.details.missing')}
                    </span>
                  </div>
                  <div>
                    <span className={styles.metaLabel}>
                      {t('agentHub:portable.details.components')}
                    </span>
                    <span data-testid="portable-plugin-components">
                      {pluginReport?.componentCount ?? 0}
                    </span>
                  </div>
                  <div>
                    <span className={styles.metaLabel}>
                      {t('agentHub:portable.details.residuals')}
                    </span>
                    <span data-testid="portable-plugin-residuals">
                      {pluginReport?.residualCount ?? 0}
                    </span>
                  </div>
                  <div>
                    <span className={styles.metaLabel}>
                      {t('agentHub:portable.details.deleteGroups')}
                    </span>
                    <span data-testid="portable-plugin-delete-groups">
                      {t('agentHub:portable.details.deleteGroupSummary', {
                        tombstone: pluginReport?.deleteTombstoneCount ?? 0,
                        preserve: pluginReport?.deletePreserveCount ?? 0,
                      })}
                    </span>
                  </div>
                </div>
              </section>
            ) : null}

            <section
              className={styles.drawerSection}
              aria-label={t('agentHub:portable.details.actionsAria')}
              data-testid="portable-asset-actions"
            >
              <div className={styles.dialogActions}>
                {canEnable ? (
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={busy}
                    onClick={() => onRequestAction('enable')}
                    data-testid="portable-action-enable"
                  >
                    {t('agentHub:portable.actions.enable')}
                  </Button>
                ) : null}
                {canDisable ? (
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={busy}
                    onClick={() => onRequestAction('disable')}
                    data-testid="portable-action-disable"
                  >
                    {t('agentHub:portable.actions.disable')}
                  </Button>
                ) : null}
                {canAdopt ? (
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={busy}
                    onClick={() => onRequestAction('adopt')}
                    data-testid="portable-action-adopt"
                  >
                    {t('agentHub:portable.actions.adopt')}
                  </Button>
                ) : null}
                {canInstall ? (
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={busy}
                    onClick={() => onRequestAction('installToSourceTarget')}
                    data-testid="portable-action-install"
                  >
                    {t('agentHub:portable.actions.installToSourceTarget')}
                  </Button>
                ) : null}
              </div>
            </section>

            <section
              className={styles.drawerSection}
              aria-label={t('agentHub:portable.details.dangerAria')}
              data-testid="portable-asset-danger-zone"
            >
              <h3 className={styles.sectionTitle}>
                {t('agentHub:portable.details.dangerTitle')}
              </h3>
              <StatusMessage tone="warn" live="off">
                {t('agentHub:portable.details.dangerHint')}
              </StatusMessage>
              {canUninstall ? (
                <Button
                  variant="danger"
                  size="sm"
                  disabled={busy}
                  onClick={() => onRequestAction('uninstall')}
                  data-testid="portable-action-uninstall"
                >
                  {t('agentHub:portable.actions.uninstall')}
                </Button>
              ) : (
                <p className={styles.hint} data-testid="portable-action-uninstall-blocked">
                  {t('agentHub:portable.details.uninstallBlocked')}
                </p>
              )}
            </section>
          </>
        )}
      </div>
    </Drawer>
  );
}

PortableAssetDetailsDrawer.displayName = 'PortableAssetDetailsDrawer';
