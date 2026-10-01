/**
 * @vitest-environment jsdom
 *
 * UserMirrorDialog pure view tests.
 *
 * Business Logic（为什么需要这个测试）:
 *   生产 Pull/Push 不得再出现 mode radio、旧逐项勾选或冲突策略；
 *   预览后提供「同步内容选择」（默认全选、跨 Agent 联动）；
 *   未预览或未勾选确认时 Apply 必须禁用。
 *
 * Code Logic（这个测试做什么）:
 *   pure props 渲染；mock i18n；断言无旧 picker 控件、资产勾选回调与 confirm 门闩 apply。
 */

import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { UserMirrorPlanDto, UserMirrorResultDto } from '@/lib/types/userMirror';
import type { UserMirrorAssetOption } from './userMirrorPresentation';
import { UserMirrorDialog } from './UserMirrorDialog';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, unknown>) =>
      opts ? `${key}:${JSON.stringify(opts)}` : key,
  }),
}));

const assetOptions: UserMirrorAssetOption[] = [
  { key: 'skill:skill-a', kind: 'skill', nativeId: 'skill-a', displayName: 'Skill A' },
  { key: 'command:cmd-x', kind: 'command', nativeId: 'cmd-x', displayName: 'Cmd X' },
];

const plan: UserMirrorPlanDto = {
  planToken: 'plan-1',
  expiresAt: '2099-01-01T00:00:00.000Z',
  direction: 'pull',
  sourceDeviceId: 'dev-a',
  destinationDeviceId: 'dev-local',
  remoteInventorySnapshotHash: 'remote-hash',
  localInventorySnapshotHash: 'local-hash',
  credentialBearingCount: 1,
  hasCredentialBearingAssets: true,
  agents: [
    {
      target: 'claude',
      instructionWrites: [
        {
          logicalId: 'claude.native.CLAUDE.md',
          op: 'replace',
          sourceHash: 'src-hash',
          destHash: 'dst-hash',
        },
      ],
      portableUpserts: [
        {
          kind: 'skill',
          nativeId: 'skill-a',
          displayName: 'Skill A',
          op: 'write',
          credentialBearing: false,
        },
      ],
      portableDeletes: [
        {
          kind: 'command',
          nativeId: 'cmd-x',
          displayName: 'Cmd X',
          op: 'delete',
          credentialBearing: false,
        },
      ],
      pluginDisables: [
        {
          kind: 'plugin',
          nativeId: 'plug-x',
          displayName: 'Plug X',
          op: 'disable',
          credentialBearing: false,
        },
      ],
      mcpDeletes: [
        {
          kind: 'mcp',
          nativeId: 'github',
          displayName: 'GitHub MCP',
          op: 'delete',
          credentialBearing: true,
        },
      ],
    },
  ],
  blockingReasons: [],
  peerPlans: [],
};

const result: UserMirrorResultDto = {
  planToken: 'plan-1',
  clientRequestId: 'req-1',
  sourceDeviceId: 'dev-a',
  destinationDeviceId: 'dev-local',
  partial: true,
  agents: [
    {
      target: 'claude',
      state: 'succeeded',
      errorCode: null,
      message: null,
    },
  ],
  peerResults: [],
};

afterEach(() => {
  cleanup();
});

describe('UserMirrorDialog', () => {
  it('has no mode radios, legacy asset pickers, or conflict policy, and confirm gates apply', () => {
    const onApply = vi.fn();
    const onConfirmChange = vi.fn();
    render(
      <UserMirrorDialog
        open
        direction="pull"
        busy={false}
        submitted={false}
        error={null}
        stale={false}
        devices={[
          { deviceId: 'device-a', name: 'Alpha' },
          { deviceId: 'device-b', name: 'Beta' },
        ]}
        sourceDeviceId="device-a"
        selectedPeerIds={[]}
        plan={plan}
        result={null}
        confirmed={false}
        canApply={false}
        canReconcile={false}
        assetOptions={assetOptions}
        selectedAssetKeys={['skill:skill-a', 'command:cmd-x']}
        includeInstructions
        onToggleAsset={vi.fn()}
        onSelectAllAssets={vi.fn()}
        onDeselectAllAssets={vi.fn()}
        onSetIncludeInstructions={vi.fn()}
        onSelectSourceDevice={vi.fn()}
        onTogglePeer={vi.fn()}
        onConfirmChange={onConfirmChange}
        onPreview={vi.fn()}
        onApply={onApply}
        onReconcile={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(screen.getByTestId('user-mirror-dialog')).toBeTruthy();
    expect(screen.getByTestId('user-mirror-lan-risk').textContent).toContain(
      'agentHub:userMirror.lanNoAuthRisk',
    );
    expect(screen.queryByTestId('lan-push-mode-fullHub')).toBeNull();
    expect(screen.queryByTestId('lan-push-mode-userScope')).toBeNull();
    expect(screen.queryByTestId('lan-push-mode-project')).toBeNull();
    expect(screen.queryByTestId('lan-push-mode-assets')).toBeNull();
    expect(screen.queryByTestId('portable-pull-item-list')).toBeNull();
    expect(screen.queryByTestId('portable-pull-filter-kind')).toBeNull();
    expect(screen.queryByTestId('portable-pull-policy-skipExisting')).toBeNull();
    expect(screen.queryByTestId('lan-push-asset-ids')).toBeNull();
    expect(screen.queryByLabelText(/conflict/i)).toBeNull();

    expect(screen.getByTestId('user-mirror-agent-claude').textContent).toContain('writes');
    expect(screen.getByTestId('user-mirror-credentials')).toBeTruthy();

    const apply = screen.getByTestId('user-mirror-apply') as HTMLButtonElement;
    expect(apply.disabled).toBe(true);
    fireEvent.click(apply);
    expect(onApply).not.toHaveBeenCalled();

    fireEvent.click(screen.getByTestId('user-mirror-confirm-overwrite'));
    expect(onConfirmChange).toHaveBeenCalledWith(true);
  });

  it('renders sync content selection with default all-selected and fires asset callbacks', () => {
    const onToggleAsset = vi.fn();
    const onSelectAllAssets = vi.fn();
    const onDeselectAllAssets = vi.fn();
    const onSetIncludeInstructions = vi.fn();
    render(
      <UserMirrorDialog
        open
        direction="pull"
        busy={false}
        submitted={false}
        error={null}
        stale={false}
        devices={[{ deviceId: 'device-a', name: 'Alpha' }]}
        sourceDeviceId="device-a"
        selectedPeerIds={[]}
        plan={plan}
        result={null}
        confirmed={false}
        canApply={false}
        canReconcile={false}
        assetOptions={assetOptions}
        selectedAssetKeys={['skill:skill-a']}
        includeInstructions
        onToggleAsset={onToggleAsset}
        onSelectAllAssets={onSelectAllAssets}
        onDeselectAllAssets={onDeselectAllAssets}
        onSetIncludeInstructions={onSetIncludeInstructions}
        onSelectSourceDevice={vi.fn()}
        onTogglePeer={vi.fn()}
        onConfirmChange={vi.fn()}
        onPreview={vi.fn()}
        onApply={vi.fn()}
        onReconcile={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    // 同步内容选择区：指令总开关默认开，资产行显示 displayName (kind)。
    expect(screen.getByTestId('user-mirror-selection')).toBeTruthy();
    const includeInstructions = screen.getByTestId(
      'user-mirror-include-instructions',
    ) as HTMLInputElement;
    expect(includeInstructions.checked).toBe(true);
    const skillBox = screen.getByTestId('user-mirror-asset-skill:skill-a') as HTMLInputElement;
    const commandBox = screen.getByTestId('user-mirror-asset-command:cmd-x') as HTMLInputElement;
    expect(skillBox.checked).toBe(true);
    expect(commandBox.checked).toBe(false);
    expect(screen.getByTestId('user-mirror-selection').textContent).toContain('Skill A');
    expect(screen.getByTestId('user-mirror-selection').textContent).toContain('skill');

    fireEvent.click(skillBox);
    expect(onToggleAsset).toHaveBeenCalledWith('skill:skill-a');
    fireEvent.click(screen.getByTestId('user-mirror-asset-select-all'));
    expect(onSelectAllAssets).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByTestId('user-mirror-asset-deselect-all'));
    expect(onDeselectAllAssets).toHaveBeenCalledTimes(1);
    fireEvent.click(includeInstructions);
    expect(onSetIncludeInstructions).toHaveBeenCalledWith(false);
  });

  it('disables repeat apply after submission and shows partial StatusMessage plus reconcile', () => {
    const onApply = vi.fn();
    const onReconcile = vi.fn();
    render(
      <UserMirrorDialog
        open
        direction="pull"
        busy={false}
        submitted
        error={null}
        stale={false}
        devices={[{ deviceId: 'device-a', name: 'Alpha' }]}
        sourceDeviceId="device-a"
        selectedPeerIds={[]}
        plan={plan}
        result={result}
        confirmed
        canApply={false}
        canReconcile
        assetOptions={assetOptions}
        selectedAssetKeys={['skill:skill-a', 'command:cmd-x']}
        includeInstructions
        onToggleAsset={vi.fn()}
        onSelectAllAssets={vi.fn()}
        onDeselectAllAssets={vi.fn()}
        onSetIncludeInstructions={vi.fn()}
        onSelectSourceDevice={vi.fn()}
        onTogglePeer={vi.fn()}
        onConfirmChange={vi.fn()}
        onPreview={vi.fn()}
        onApply={onApply}
        onReconcile={onReconcile}
        onClose={vi.fn()}
      />,
    );

    const apply = screen.getByTestId('user-mirror-apply') as HTMLButtonElement;
    expect(apply.disabled).toBe(true);
    fireEvent.click(apply);
    expect(onApply).not.toHaveBeenCalled();
    expect((screen.getByTestId('user-mirror-include-instructions') as HTMLInputElement).disabled).toBe(
      true,
    );
    expect(screen.getByTestId('user-mirror-partial')).toBeTruthy();
    fireEvent.click(screen.getByTestId('user-mirror-reconcile'));
    expect(onReconcile).toHaveBeenCalledTimes(1);
  });

  it('push lists peer checkboxes without asset-id mode inputs and keeps a report region', () => {
    const onTogglePeer = vi.fn();
    render(
      <UserMirrorDialog
        open
        direction="push"
        busy={false}
        submitted
        error={null}
        stale={false}
        devices={[
          { deviceId: 'peer-a', name: 'Alpha' },
          { deviceId: 'peer-b', name: 'Beta' },
        ]}
        sourceDeviceId=""
        selectedPeerIds={['peer-a']}
        plan={null}
        result={{
          ...result,
          destinationDeviceId: 'peer-a',
          partial: true,
          agents: [],
          peerResults: [
            {
              destinationDeviceId: 'peer-a',
              partial: true,
              agents: result.agents,
            },
            {
              destinationDeviceId: 'peer-b',
              partial: false,
              agents: [
                {
                  target: 'codex',
                  state: 'failed',
                  errorCode: 'WRITE_FAILED',
                  message: 'disk full',
                },
              ],
            },
          ],
        }}
        confirmed={false}
        canApply={false}
        canReconcile
        assetOptions={[]}
        selectedAssetKeys={[]}
        includeInstructions
        onToggleAsset={vi.fn()}
        onSelectAllAssets={vi.fn()}
        onDeselectAllAssets={vi.fn()}
        onSetIncludeInstructions={vi.fn()}
        onSelectSourceDevice={vi.fn()}
        onTogglePeer={onTogglePeer}
        onConfirmChange={vi.fn()}
        onPreview={vi.fn()}
        onApply={vi.fn()}
        onReconcile={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(screen.queryByTestId('lan-push-mode-fullHub')).toBeNull();
    expect(screen.queryByTestId('lan-push-asset-ids')).toBeNull();
    fireEvent.click(screen.getByTestId('user-mirror-peer-peer-b'));
    expect(onTogglePeer).toHaveBeenCalledWith('peer-b');
    expect(screen.getByTestId('user-mirror-report')).toBeTruthy();
    expect(screen.getByTestId('user-mirror-report-peer-a')).toBeTruthy();
    expect(screen.getByTestId('user-mirror-report-peer-b').textContent).toContain('Beta');
    expect(screen.getByTestId('user-mirror-report-peer-b').textContent).toContain(
      'agentHub:userMirror.itemState.failed',
    );
    expect(screen.getByTestId('user-mirror-report-peer-b').textContent).toContain('WRITE_FAILED');
  });

  it('projects per-agent counts and credential disclosure through the selected scope', () => {
    render(
      <UserMirrorDialog
        open
        direction="pull"
        busy={false}
        submitted={false}
        error={null}
        stale={false}
        devices={[{ deviceId: 'dev-a', name: 'Alpha' }]}
        sourceDeviceId="dev-a"
        selectedPeerIds={[]}
        plan={plan}
        result={null}
        confirmed={false}
        canApply={false}
        canReconcile={false}
        assetOptions={assetOptions}
        selectedAssetKeys={['skill:skill-a']}
        includeInstructions={false}
        onToggleAsset={vi.fn()}
        onSelectAllAssets={vi.fn()}
        onDeselectAllAssets={vi.fn()}
        onSetIncludeInstructions={vi.fn()}
        onSelectSourceDevice={vi.fn()}
        onTogglePeer={vi.fn()}
        onConfirmChange={vi.fn()}
        onPreview={vi.fn()}
        onApply={vi.fn()}
        onReconcile={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    const counts = screen.getByTestId('user-mirror-agent-claude').textContent;
    expect(counts).toContain('"writes":0');
    expect(counts).toContain('"upserts":1');
    expect(counts).toContain('"deletes":0');
    expect(counts).toContain('"disables":0');
    expect(screen.getByTestId('user-mirror-credentials').hidden).toBe(true);
  });

  it('renders an independent preview for every push destination', () => {
    const peerPlan = {
      destinationDeviceId: 'peer-a',
      remoteInventorySnapshotHash: 'peer-a-hash',
      agents: plan.agents,
      blockingReasons: [],
    };
    render(
      <UserMirrorDialog
        open
        direction="push"
        busy={false}
        submitted={false}
        error={null}
        stale={false}
        devices={[
          { deviceId: 'peer-a', name: 'Alpha' },
          { deviceId: 'peer-b', name: 'Beta' },
        ]}
        sourceDeviceId=""
        selectedPeerIds={['peer-a', 'peer-b']}
        plan={{
          ...plan,
          direction: 'push',
          sourceDeviceId: 'local',
          destinationDeviceId: '',
          agents: [],
          blockingReasons: [],
          peerPlans: [peerPlan, { ...peerPlan, destinationDeviceId: 'peer-b' }],
        }}
        result={null}
        confirmed={false}
        canApply={false}
        canReconcile={false}
        assetOptions={assetOptions}
        selectedAssetKeys={['skill:skill-a', 'command:cmd-x']}
        includeInstructions
        onToggleAsset={vi.fn()}
        onSelectAllAssets={vi.fn()}
        onDeselectAllAssets={vi.fn()}
        onSetIncludeInstructions={vi.fn()}
        onSelectSourceDevice={vi.fn()}
        onTogglePeer={vi.fn()}
        onConfirmChange={vi.fn()}
        onPreview={vi.fn()}
        onApply={vi.fn()}
        onReconcile={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(screen.getByTestId('user-mirror-peer-plan-peer-a').textContent).toContain('Alpha');
    expect(screen.getByTestId('user-mirror-peer-plan-peer-b').textContent).toContain('Beta');
    expect(screen.getByTestId('user-mirror-peer-plan-peer-a').textContent).toContain(
      'agentHub:userMirror.directionToPeer',
    );
  });

  it('keeps the action footer outside the single scroll region', () => {
    render(
      <UserMirrorDialog
        open
        direction="pull"
        busy={false}
        submitted={false}
        error={null}
        stale={false}
        devices={[{ deviceId: 'dev-a', name: 'Alpha' }]}
        sourceDeviceId="dev-a"
        selectedPeerIds={[]}
        plan={plan}
        result={null}
        confirmed={false}
        canApply={false}
        canReconcile={false}
        assetOptions={assetOptions}
        selectedAssetKeys={['skill:skill-a', 'command:cmd-x']}
        includeInstructions
        onToggleAsset={vi.fn()}
        onSelectAllAssets={vi.fn()}
        onDeselectAllAssets={vi.fn()}
        onSetIncludeInstructions={vi.fn()}
        onSelectSourceDevice={vi.fn()}
        onTogglePeer={vi.fn()}
        onConfirmChange={vi.fn()}
        onPreview={vi.fn()}
        onApply={vi.fn()}
        onReconcile={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    const scrollRegion = screen.getByTestId('user-mirror-scroll-region');
    const footer = screen.getByTestId('user-mirror-footer');
    expect(scrollRegion.contains(footer)).toBe(false);
    expect(scrollRegion.parentElement).toBe(footer.parentElement);
  });
});
