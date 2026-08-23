/**
 * @vitest-environment jsdom
 *
 * UserMirrorDialog pure view tests.
 *
 * Business Logic（为什么需要这个测试）:
 *   生产 Pull/Push 不得再出现 mode radio、库存勾选或冲突策略；
 *   未预览或未勾选确认时 Apply 必须禁用。
 *
 * Code Logic（这个测试做什么）:
 *   pure props 渲染；mock i18n；断言无旧 picker 控件且 confirm 门闩 apply。
 */

import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { UserMirrorPlanDto, UserMirrorResultDto } from '@/lib/types/userMirror';
import { UserMirrorDialog } from './UserMirrorDialog';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, unknown>) =>
      opts ? `${key}:${JSON.stringify(opts)}` : key,
  }),
}));

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
};

afterEach(() => {
  cleanup();
});

describe('UserMirrorDialog', () => {
  it('has no mode radios, inventory checkboxes, or conflict policy, and confirm gates apply', () => {
    const onApply = vi.fn();
    const onConfirmChange = vi.fn();
    render(
      <UserMirrorDialog
        open
        direction="pull"
        busy={false}
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

  it('enables apply after confirm and shows partial StatusMessage plus reconcile', () => {
    const onApply = vi.fn();
    const onReconcile = vi.fn();
    render(
      <UserMirrorDialog
        open
        direction="pull"
        busy={false}
        error={null}
        stale={false}
        devices={[{ deviceId: 'device-a', name: 'Alpha' }]}
        sourceDeviceId="device-a"
        selectedPeerIds={[]}
        plan={plan}
        result={result}
        confirmed
        canApply
        canReconcile
        onSelectSourceDevice={vi.fn()}
        onTogglePeer={vi.fn()}
        onConfirmChange={vi.fn()}
        onPreview={vi.fn()}
        onApply={onApply}
        onReconcile={onReconcile}
        onClose={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByTestId('user-mirror-apply'));
    expect(onApply).toHaveBeenCalledTimes(1);
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
        error={null}
        stale={false}
        devices={[
          { deviceId: 'peer-a', name: 'A' },
          { deviceId: 'peer-b', name: 'B' },
        ]}
        sourceDeviceId=""
        selectedPeerIds={['peer-a']}
        plan={null}
        result={{
          ...result,
          destinationDeviceId: 'peer-a',
          partial: true,
        }}
        confirmed={false}
        canApply={false}
        canReconcile
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
  });
});
