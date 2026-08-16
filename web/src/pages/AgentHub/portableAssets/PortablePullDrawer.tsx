/**
 * Same-agent remote portable pull Drawer — pure view.
 *
 * Business Logic（为什么需要这个组件）:
 *   用户选择远端设备 + 同类 source Agent，筛选勾选远端 inventory，preview/apply Pull；
 *   destination 固定为 sourceTarget；展示 LAN no-auth 风险、credential boolean、
 *   canonical-only mapping 与 per-item progress；禁止跨 Agent destination picker。
 *
 * Code Logic（这个组件做什么）:
 *   pure props 视图；hooks 仅 useTranslation/useMemo；无 @/api/*。
 */

import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Drawer, Input, Pill, StatusMessage } from '@/components/primitives';
import { allHubTargets } from '@/lib/agentCatalog';
import type { AgentTarget } from '@/lib/types/agentHub';
import type { Device } from '@/lib/types';
import type {
  PortableAssetConflictPolicy,
  PortableAssetKind,
  PortablePullPlanDto,
  PortablePullResultDto,
  RemotePortableInventoryDto,
  RemotePortableInventoryItemDto,
} from '@/lib/types/portableInventory';
import styles from '../AgentHub.module.css';
import {
  credentialDisclosureFromPlan,
  formatPullInstallModeLabelKey,
  mapCanonicalOnlyChanges,
  portablePullItemResultTone,
  sameAgentDestinationLabelKey,
  summarizeConflictPolicyDiff,
  summarizePullResultProgress,
  type PortablePullFilters,
} from './portablePullPresentation';

export interface PortablePullDrawerProps {
  open: boolean;
  busy: boolean;
  error: string | null;
  devices: Device[];
  selectedDeviceId: string;
  sourceTarget: AgentTarget;
  remoteInventory: RemotePortableInventoryDto | null;
  visibleItems: RemotePortableInventoryItemDto[];
  selectedItemIds: Set<string>;
  filters: PortablePullFilters;
  conflictPolicy: PortableAssetConflictPolicy;
  plan: PortablePullPlanDto | null;
  result: PortablePullResultDto | null;
  mutationBlocked: boolean;
  canApply: boolean;
  canReconcile: boolean;
  onSelectDevice: (deviceId: string) => void;
  onSelectSourceTarget: (target: AgentTarget) => void;
  onSetFilters: (filters: PortablePullFilters) => void;
  onToggleItem: (inventoryItemId: string) => void;
  onSelectVisible: () => void;
  onSetConflictPolicy: (policy: PortableAssetConflictPolicy) => void;
  onLoadInventory: () => void;
  onPreview: () => void;
  onApply: () => void;
  onReconcile: () => void;
  onClose: () => void;
}

function tr(t: (key: never) => string, key: string): string {
  return t(key as never);
}

const KIND_OPTIONS: Array<'all' | PortableAssetKind> = [
  'all',
  'skill',
  'command',
  'plugin',
  'mcp',
];
const SCOPE_OPTIONS: Array<PortablePullFilters['scope']> = ['all', 'user', 'project'];
const STATE_OPTIONS: Array<PortablePullFilters['actualState']> = [
  'all',
  'enabled',
  'disabled',
  'problem',
];
const TARGET_OPTIONS: AgentTarget[] = allHubTargets();

/**
 * Business Logic: pure same-agent Pull drawer。
 * Code Logic: Drawer + device/target + filters + selection + preview/apply/reconcile。
 */
export function PortablePullDrawer(props: PortablePullDrawerProps) {
  const { t } = useTranslation(['agentHub', 'common'] as const);
  const {
    open,
    busy,
    error,
    devices,
    selectedDeviceId,
    sourceTarget,
    remoteInventory,
    visibleItems,
    selectedItemIds,
    filters,
    conflictPolicy,
    plan,
    result,
    mutationBlocked,
    canApply,
    canReconcile,
    onSelectDevice,
    onSelectSourceTarget,
    onSetFilters,
    onToggleItem,
    onSelectVisible,
    onSetConflictPolicy,
    onLoadInventory,
    onPreview,
    onApply,
    onReconcile,
    onClose,
  } = props;

  const selectedSet = useMemo(() => selectedItemIds ?? new Set<string>(), [selectedItemIds]);
  const credential = useMemo(() => credentialDisclosureFromPlan(plan), [plan]);
  const canonicalOnly = useMemo(
    () => mapCanonicalOnlyChanges(plan?.changes ?? []),
    [plan],
  );
  const conflictDiff = useMemo(
    () => summarizeConflictPolicyDiff(conflictPolicy, plan?.changes ?? []),
    [conflictPolicy, plan],
  );
  const progress = useMemo(() => summarizePullResultProgress(result), [result]);
  const destinationLabelKey = sameAgentDestinationLabelKey(sourceTarget);

  // hooks 全部在 early return 前
  if (!open) return null;

  return (
    <Drawer
      open={open}
      titleId="portable-pull-title"
      onClose={busy ? () => undefined : onClose}
      side="right"
      closeOnEscape={!busy}
      closeOnBackdrop={!busy}
      className={styles.drawerSurface}
    >
      <div className={styles.replicationBody} data-testid="portable-pull-drawer">
        <h2 id="portable-pull-title" className={styles.drawerTitle}>
          {t('agentHub:portablePull.title')}
        </h2>
        <p className={styles.replicationHint}>{t('agentHub:portablePull.hint')}</p>
        <p
          className={styles.replicationDisclosure}
          data-testid="portable-pull-lan-risk"
        >
          {t('agentHub:portablePull.lanNoAuthRisk')}
        </p>

        {error ? (
          <StatusMessage tone="danger" data-testid="portable-pull-error">
            {error}
          </StatusMessage>
        ) : null}

        {remoteInventory?.stale || mutationBlocked ? (
          <StatusMessage tone="warn" data-testid="portable-pull-stale-banner">
            {t('agentHub:portablePull.staleInventory')}
          </StatusMessage>
        ) : null}

        <section aria-label={t('agentHub:portablePull.deviceSectionAria')}>
          <h3 className={styles.replicationSectionTitle}>
            {t('agentHub:portablePull.deviceSection')}
          </h3>
          <label className={styles.replicationCheckRow}>
            <span>{t('agentHub:portablePull.deviceLabel')}</span>
            <select
              value={selectedDeviceId}
              disabled={busy}
              onChange={(e) => onSelectDevice(e.target.value)}
              data-testid="portable-pull-device"
              aria-label={t('agentHub:portablePull.deviceLabel')}
            >
              {devices.length === 0 ? (
                <option value="">{t('agentHub:portablePull.noDevices')}</option>
              ) : null}
              {devices.map((device) => (
                <option key={device.id} value={device.id}>
                  {device.name} ({device.id})
                </option>
              ))}
            </select>
          </label>

          <label className={styles.replicationCheckRow}>
            <span>{t('agentHub:portablePull.sourceTargetLabel')}</span>
            <select
              value={sourceTarget}
              disabled={busy}
              onChange={(e) => onSelectSourceTarget(e.target.value as AgentTarget)}
              data-testid="portable-pull-source-target"
              aria-label={t('agentHub:portablePull.sourceTargetLabel')}
            >
              {TARGET_OPTIONS.map((target) => (
                <option key={target} value={target}>
                  {t(`agentHub:portablePull.targets.${target}`)}
                </option>
              ))}
            </select>
          </label>

          <p data-testid="portable-pull-same-target">
            {tr(t, destinationLabelKey)}
          </p>
        </section>

        <section aria-label={t('agentHub:portablePull.filtersAria')}>
          <h3 className={styles.replicationSectionTitle}>
            {t('agentHub:portablePull.filters')}
          </h3>
          <div className={styles.replicationModeRow}>
            <label>
              <span>{t('agentHub:portablePull.filterKind')}</span>
              <select
                value={filters.kind}
                disabled={busy}
                data-testid="portable-pull-filter-kind"
                onChange={(e) =>
                  onSetFilters({
                    ...filters,
                    kind: e.target.value as PortablePullFilters['kind'],
                  })
                }
              >
                {KIND_OPTIONS.map((kind) => (
                  <option key={kind} value={kind}>
                    {t(`agentHub:portablePull.kinds.${kind}`)}
                  </option>
                ))}
              </select>
            </label>
            <label>
              <span>{t('agentHub:portablePull.filterScope')}</span>
              <select
                value={filters.scope}
                disabled={busy}
                data-testid="portable-pull-filter-scope"
                onChange={(e) =>
                  onSetFilters({
                    ...filters,
                    scope: e.target.value as PortablePullFilters['scope'],
                  })
                }
              >
                {SCOPE_OPTIONS.map((scope) => (
                  <option key={scope} value={scope}>
                    {t(`agentHub:portablePull.scopes.${scope}`)}
                  </option>
                ))}
              </select>
            </label>
            <label>
              <span>{t('agentHub:portablePull.filterState')}</span>
              <select
                value={filters.actualState}
                disabled={busy}
                data-testid="portable-pull-filter-state"
                onChange={(e) =>
                  onSetFilters({
                    ...filters,
                    actualState: e.target.value as PortablePullFilters['actualState'],
                  })
                }
              >
                {STATE_OPTIONS.map((state) => (
                  <option key={state} value={state}>
                    {t(`agentHub:portablePull.states.${state}`)}
                  </option>
                ))}
              </select>
            </label>
          </div>
          <Input
            value={filters.search}
            disabled={busy}
            data-testid="portable-pull-filter-search"
            placeholder={t('agentHub:portablePull.searchPlaceholder')}
            aria-label={t('agentHub:portablePull.searchPlaceholder')}
            onChange={(e) => onSetFilters({ ...filters, search: e.target.value })}
          />
        </section>

        <section aria-label={t('agentHub:portablePull.inventoryAria')}>
          <h3 className={styles.replicationSectionTitle}>
            {t('agentHub:portablePull.inventory')}
          </h3>
          <div className={styles.replicationModeRow}>
            <Button
              type="button"
              variant="secondary"
              disabled={busy || !selectedDeviceId}
              onClick={onLoadInventory}
              data-testid="portable-pull-load"
            >
              {t('agentHub:portablePull.loadInventory')}
            </Button>
            <Button
              type="button"
              variant="secondary"
              disabled={busy || visibleItems.length === 0}
              onClick={onSelectVisible}
              data-testid="portable-pull-select-visible"
            >
              {t('agentHub:portablePull.selectVisible')}
            </Button>
          </div>
          {remoteInventory ? (
            <ul className={styles.replicationList} data-testid="portable-pull-item-list">
              {visibleItems.map((item) => (
                <li key={item.inventoryItemId}>
                  <label className={styles.replicationCheckRow}>
                    <input
                      type="checkbox"
                      checked={selectedSet.has(item.inventoryItemId)}
                      disabled={busy}
                      onChange={() => onToggleItem(item.inventoryItemId)}
                      data-testid={`portable-pull-item-${item.inventoryItemId}`}
                    />
                    <span>
                      {item.displayName} · {item.kind}
                      {item.actualEnabled === true
                        ? ` · ${t('agentHub:portablePull.states.enabled')}`
                        : item.actualEnabled === false
                          ? ` · ${t('agentHub:portablePull.states.disabled')}`
                          : ` · ${t('agentHub:portablePull.states.problem')}`}
                      {item.mcpCredential?.present
                        ? ` · ${t('agentHub:replication.credentialLabel')}`
                        : ''}
                    </span>
                  </label>
                </li>
              ))}
            </ul>
          ) : (
            <StatusMessage tone="info">{t('agentHub:portablePull.loadHint')}</StatusMessage>
          )}
        </section>

        <section aria-label={t('agentHub:portablePull.policyAria')}>
          <h3 className={styles.replicationSectionTitle}>
            {t('agentHub:portablePull.conflictPolicy')}
          </h3>
          <div className={styles.replicationModeRow} role="radiogroup">
            {(
              [
                ['skipExisting', 'agentHub:portablePull.policy.skipExisting'],
                ['replaceAfterPreview', 'agentHub:portablePull.policy.replaceAfterPreview'],
              ] as const
            ).map(([value, labelKey]) => (
              <label key={value} className={styles.replicationCheckRow}>
                <input
                  type="radio"
                  name="portable-pull-conflict-policy"
                  value={value}
                  checked={conflictPolicy === value}
                  disabled={busy}
                  onChange={() => onSetConflictPolicy(value)}
                  data-testid={`portable-pull-policy-${value}`}
                />
                <span>{t(labelKey)}</span>
              </label>
            ))}
          </div>
        </section>

        <section data-testid="portable-pull-preview-step">
          <h3 className={styles.replicationSectionTitle}>
            {t('agentHub:portablePull.preview')}
          </h3>
          <Button
            type="button"
            variant="secondary"
            disabled={busy || mutationBlocked || selectedSet.size === 0 || !remoteInventory}
            onClick={onPreview}
            data-testid="portable-pull-preview-btn"
          >
            {t('agentHub:portablePull.previewAction')}
          </Button>
          {plan ? (
            <div data-testid="portable-pull-preview">
              <p>
                {t('agentHub:portablePull.previewSummary', {
                  count: plan.changes.length,
                  hash: plan.selectionManifestHash.slice(0, 12),
                })}
              </p>
              <p data-testid="portable-pull-conflict-diff">
                {t('agentHub:portablePull.conflictDiff', {
                  conflicts: conflictDiff.conflictCount,
                  skipped: conflictDiff.skippedByPolicy,
                  replace: conflictDiff.replaceCandidates,
                })}
              </p>
              {credential.hasCredentialBearingAssets ? (
                <Pill tone="warn" data-testid="portable-pull-credential-disclosure">
                  {t('agentHub:replication.hasCredentials')}
                  {` · ${credential.credentialBearingCount}`}
                </Pill>
              ) : (
                <span data-testid="portable-pull-credential-disclosure">
                  {t('agentHub:portablePull.noCredentials')}
                </span>
              )}
              {canonicalOnly.length > 0 ? (
                <div data-testid="portable-pull-canonical-only">
                  <p>{t('agentHub:portablePull.canonicalOnlyHint')}</p>
                  <ul className={styles.replicationList}>
                    {canonicalOnly.map((change) => (
                      <li key={change.inventoryItemId}>
                        {change.displayName} · {tr(t, formatPullInstallModeLabelKey(change.installMode))}
                      </li>
                    ))}
                  </ul>
                </div>
              ) : (
                <div data-testid="portable-pull-canonical-only" hidden />
              )}
              <ul className={styles.replicationList}>
                {plan.changes.map((change) => (
                  <li key={change.inventoryItemId} data-testid={`portable-pull-change-${change.inventoryItemId}`}>
                    {change.displayName} · {tr(t, formatPullInstallModeLabelKey(change.installMode))}
                    {change.conflict ? ` · ${t('agentHub:portablePull.conflictLabel')}` : ''}
                    {change.credentialBearing
                      ? ` · ${t('agentHub:replication.credentialLabel')}`
                      : ''}
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
        </section>

        <section data-testid="portable-pull-apply-step">
          <h3 className={styles.replicationSectionTitle}>
            {t('agentHub:portablePull.apply')}
          </h3>
          <div className={styles.replicationModeRow}>
            <Button
              type="button"
              variant="primary"
              disabled={busy || !canApply}
              onClick={onApply}
              data-testid="portable-pull-apply"
            >
              {t('agentHub:portablePull.applyAction')}
            </Button>
            {canReconcile ? (
              <Button
                type="button"
                variant="secondary"
                disabled={busy}
                onClick={onReconcile}
                data-testid="portable-pull-reconcile"
              >
                {t('agentHub:portablePull.reconcileAction')}
              </Button>
            ) : null}
          </div>
          {result ? (
            <div data-testid="portable-pull-result">
              <p data-testid="portable-pull-progress">
                {t('agentHub:portablePull.progressSummary', {
                  total: progress.total,
                  succeeded: progress.succeeded,
                  skipped: progress.skipped,
                  failed: progress.failed,
                  blocked: progress.blocked,
                  unknown: progress.outcomeUnknown,
                })}
              </p>
              <ul className={styles.replicationList}>
                {result.items.map((item) => (
                  <li
                    key={item.inventoryItemId}
                    data-testid={`portable-pull-result-${item.inventoryItemId}`}
                  >
                    <Pill tone={portablePullItemResultTone(item.state)}>{item.state}</Pill>
                    <span>
                      {' '}
                      {item.inventoryItemId}
                      {item.installMode
                        ? ` · ${tr(t, formatPullInstallModeLabelKey(item.installMode))}`
                        : ''}
                      {item.errorCode ? ` · ${item.errorCode}` : ''}
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
        </section>
      </div>
    </Drawer>
  );
}
