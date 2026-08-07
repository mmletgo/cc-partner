/**
 * @vitest-environment jsdom
 *
 * PortablePullDrawer pure view tests.
 *
 * Business Logic（为什么需要这个测试）:
 *   Drawer 必须展示 same-target 标签、四类筛选、credential boolean、
 *   LAN no-auth risk copy、canonical-only mapping 与 per-item progress，
 *   且不提供跨 Agent destination picker。
 *
 * Code Logic（这个测试做什么）:
 *   pure props 渲染；mock i18n；断言 testids 与 callback。
 */

import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { Device } from '@/lib/types';
import type {
  PortablePullPlanDto,
  PortablePullResultDto,
  RemotePortableInventoryDto,
} from '@/lib/types/portableInventory';
import { PortablePullDrawer } from './PortablePullDrawer';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, unknown>) =>
      opts ? `${key}:${JSON.stringify(opts)}` : key,
  }),
}));

const devices: Device[] = [
  {
    id: 'device-a',
    name: 'Alpha',
    address: '10.0.0.2',
    port: 62116,
    status: 'online',
  },
];

const remoteInventory: RemotePortableInventoryDto = {
  sourceDeviceId: 'device-a',
  sourceTarget: 'claude',
  inventorySnapshotHash: 'remote-snap-1',
  refreshedAt: '2026-08-08T00:00:00.000Z',
  stale: false,
  items: [
    {
      inventoryItemId: 'remote-skill-1',
      target: 'claude',
      kind: 'skill',
      nativeId: 'remote-skill',
      displayName: 'Remote Skill',
      description: null,
      version: '1.0.0',
      scopeId: 'user',
      projectId: null,
      projectOptedIn: true,
      sourceOrigin: 'standalone',
      actualEnabled: true,
      contentHash: 'c1',
      treeHash: null,
      warnings: [],
    },
    {
      inventoryItemId: 'remote-mcp-1',
      target: 'claude',
      kind: 'mcp',
      nativeId: 'remote-mcp',
      displayName: 'Remote MCP',
      description: null,
      version: null,
      scopeId: 'user',
      projectId: null,
      projectOptedIn: true,
      sourceOrigin: 'standalone',
      actualEnabled: true,
      contentHash: 'c2',
      treeHash: null,
      warnings: [],
      mcpCredential: { present: true, hash: 'cred-hash' },
    },
  ],
};

const plan: PortablePullPlanDto = {
  planToken: 'pull-plan-1',
  expiresAt: '2026-08-08T00:15:00.000Z',
  sourceDeviceId: 'device-a',
  sourceTarget: 'claude',
  destinationTarget: 'claude',
  remoteInventorySnapshotHash: 'remote-snap-1',
  localInventorySnapshotHash: 'local-snap-1',
  conflictPolicy: 'skipExisting',
  selectionManifestHash: 'sel-1',
  credentialBearingCount: 1,
  hasCredentialBearingAssets: true,
  changes: [
    {
      inventoryItemId: 'remote-skill-1',
      kind: 'skill',
      nativeId: 'remote-skill',
      displayName: 'Remote Skill',
      installMode: 'installToTarget',
      conflict: false,
      legacyLossy: false,
      credentialBearing: false,
      blockingReasons: [],
      warnings: [],
    },
    {
      inventoryItemId: 'remote-mcp-1',
      kind: 'mcp',
      nativeId: 'remote-mcp',
      displayName: 'Remote MCP',
      installMode: 'importedCanonicalOnly',
      conflict: false,
      legacyLossy: false,
      credentialBearing: true,
      blockingReasons: ['projectUnmapped'],
      warnings: [],
    },
  ],
  blockingReasons: [],
};

const result: PortablePullResultDto = {
  planToken: 'pull-plan-1',
  clientRequestId: 'req-1',
  sourceDeviceId: 'device-a',
  sourceTarget: 'claude',
  destinationTarget: 'claude',
  partial: true,
  items: [
    {
      inventoryItemId: 'remote-skill-1',
      state: 'succeeded',
      installMode: 'installToTarget',
      errorCode: null,
      message: null,
    },
    {
      inventoryItemId: 'remote-mcp-1',
      state: 'outcomeUnknown',
      installMode: 'importedCanonicalOnly',
      errorCode: 'timeout',
      message: null,
    },
  ],
};

describe('PortablePullDrawer', () => {
  afterEach(() => {
    cleanup();
  });

  it('renders same-target destination, filters, risk copy, credential boolean and canonical-only mapping', () => {
    const onSelectDevice = vi.fn();
    const onSelectSourceTarget = vi.fn();
    const onToggleItem = vi.fn();
    const onSelectVisible = vi.fn();
    const onPreview = vi.fn();
    const onApply = vi.fn();
    const onReconcile = vi.fn();
    const onSetFilters = vi.fn();
    const onSetConflictPolicy = vi.fn();
    const onLoadInventory = vi.fn();

    render(
      <PortablePullDrawer
        open
        busy={false}
        error={null}
        devices={devices}
        selectedDeviceId="device-a"
        sourceTarget="claude"
        remoteInventory={remoteInventory}
        visibleItems={remoteInventory.items}
        selectedItemIds={new Set(['remote-skill-1', 'remote-mcp-1'])}
        filters={{ kind: 'all', scope: 'all', actualState: 'all', search: '' }}
        conflictPolicy="skipExisting"
        plan={plan}
        result={result}
        mutationBlocked={false}
        canApply
        canReconcile
        onSelectDevice={onSelectDevice}
        onSelectSourceTarget={onSelectSourceTarget}
        onSetFilters={onSetFilters}
        onToggleItem={onToggleItem}
        onSelectVisible={onSelectVisible}
        onSetConflictPolicy={onSetConflictPolicy}
        onLoadInventory={onLoadInventory}
        onPreview={onPreview}
        onApply={onApply}
        onReconcile={onReconcile}
        onClose={vi.fn()}
      />,
    );

    expect(screen.getByTestId('portable-pull-drawer')).toBeTruthy();
    expect(screen.getByTestId('portable-pull-lan-risk').textContent).toContain(
      'agentHub:portablePull.lanNoAuthRisk',
    );
    expect(screen.getByTestId('portable-pull-same-target').textContent).toContain(
      'agentHub:portablePull.destination.sameAsClaude',
    );
    // no cross-agent destination control
    expect(screen.queryByTestId('portable-pull-destination-picker')).toBeNull();
    expect(screen.queryByLabelText(/destination agent/i)).toBeNull();

    expect(screen.getByTestId('portable-pull-filter-kind')).toBeTruthy();
    expect(screen.getByTestId('portable-pull-filter-scope')).toBeTruthy();
    expect(screen.getByTestId('portable-pull-filter-state')).toBeTruthy();
    expect(screen.getByTestId('portable-pull-filter-search')).toBeTruthy();

    expect(screen.getByTestId('portable-pull-credential-disclosure').textContent).toMatch(
      /hasCredentials|credentialBearing/i,
    );
    expect(screen.getByTestId('portable-pull-credential-disclosure').textContent).not.toMatch(
      /secret|token=|api[_-]?key/i,
    );
    expect(screen.getByTestId('portable-pull-canonical-only').textContent).toContain(
      'importedCanonicalOnly',
    );

    fireEvent.change(screen.getByTestId('portable-pull-device'), {
      target: { value: 'device-a' },
    });
    fireEvent.change(screen.getByTestId('portable-pull-source-target'), {
      target: { value: 'codex' },
    });
    expect(onSelectSourceTarget).toHaveBeenCalledWith('codex');

    fireEvent.click(screen.getByTestId('portable-pull-select-visible'));
    expect(onSelectVisible).toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('portable-pull-item-remote-skill-1'));
    expect(onToggleItem).toHaveBeenCalledWith('remote-skill-1');

    fireEvent.click(screen.getByTestId('portable-pull-policy-replaceAfterPreview'));
    expect(onSetConflictPolicy).toHaveBeenCalledWith('replaceAfterPreview');

    fireEvent.click(screen.getByTestId('portable-pull-load'));
    fireEvent.click(screen.getByTestId('portable-pull-preview-btn'));
    fireEvent.click(screen.getByTestId('portable-pull-apply'));
    fireEvent.click(screen.getByTestId('portable-pull-reconcile'));
    expect(onLoadInventory).toHaveBeenCalled();
    expect(onPreview).toHaveBeenCalled();
    expect(onApply).toHaveBeenCalled();
    expect(onReconcile).toHaveBeenCalled();

    expect(screen.getByTestId('portable-pull-result-remote-skill-1').textContent).toContain(
      'succeeded',
    );
    expect(screen.getByTestId('portable-pull-result-remote-mcp-1').textContent).toContain(
      'outcomeUnknown',
    );
    const progressText = screen.getByTestId('portable-pull-progress').textContent ?? '';
    expect(progressText).toContain('agentHub:portablePull.progressSummary');
    expect(progressText).toContain('"succeeded":1');
    expect(progressText).toContain('"skipped":0');
    expect(progressText).toContain('"failed":0');
    expect(progressText).toContain('"blocked":0');
    expect(progressText).toContain('"unknown":1');
    expect(progressText).toContain('"total":2');
    expect(progressText).not.toContain('outcomeUnknown');
  });

  it('disables confirm when mutationBlocked or cannot apply', () => {
    render(
      <PortablePullDrawer
        open
        busy={false}
        error={null}
        devices={devices}
        selectedDeviceId="device-a"
        sourceTarget="claude"
        remoteInventory={{ ...remoteInventory, stale: true }}
        visibleItems={remoteInventory.items}
        selectedItemIds={new Set(['remote-skill-1'])}
        filters={{ kind: 'all', scope: 'all', actualState: 'all', search: '' }}
        conflictPolicy="skipExisting"
        plan={plan}
        result={null}
        mutationBlocked
        canApply={false}
        canReconcile={false}
        onSelectDevice={vi.fn()}
        onSelectSourceTarget={vi.fn()}
        onSetFilters={vi.fn()}
        onToggleItem={vi.fn()}
        onSelectVisible={vi.fn()}
        onSetConflictPolicy={vi.fn()}
        onLoadInventory={vi.fn()}
        onPreview={vi.fn()}
        onApply={vi.fn()}
        onReconcile={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect((screen.getByTestId('portable-pull-apply') as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByTestId('portable-pull-stale-banner')).toBeTruthy();
  });
});
