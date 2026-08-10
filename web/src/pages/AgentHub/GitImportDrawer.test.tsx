/**
 * @vitest-environment jsdom
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { GitImportDrawer } from './GitImportDrawer';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, unknown>) =>
      opts ? `${key}:${JSON.stringify(opts)}` : key,
  }),
}));

afterEach(() => cleanup());

describe('GitImportDrawer', () => {
  it('separates inspect, preview and confirm; requires explicit mapping for unmapped projects', () => {
    const onInspect = vi.fn();
    const onSelectLane = vi.fn();
    const onPreview = vi.fn();
    const onConfirmMapping = vi.fn();
    const onConfirmImport = vi.fn();
    const onMappingDraftChange = vi.fn();

    render(
      <GitImportDrawer
        open
        busy={false}
        error={null}
        inspectReport={{
          workdirPresent: true,
          localDeviceId: 'local',
          lanes: [
            {
              laneDeviceId: 'device-b',
              snapshotHash: 'hashhashhash',
              snapshotId: 's1',
              sourceReplicaId: 'device-b',
              assetCount: 2,
              revisionCount: 2,
              status: 'ok',
            },
            {
              laneDeviceId: 'device-bad',
              snapshotHash: '',
              snapshotId: '',
              sourceReplicaId: '',
              assetCount: 0,
              revisionCount: 0,
              status: 'corrupt',
              errorCode: 'bad',
            },
          ],
        }}
        selectedLaneDeviceId="device-b"
        preview={{
          laneDeviceId: 'device-b',
          snapshotId: 's1',
          snapshotHash: 'hashhashhash',
          sourceReplicaId: 'device-b',
          assetCount: 1,
          revisionCount: 1,
          changeCounts: {
            added: 1,
            modified: 0,
            deleted: 0,
            conflict: 0,
            unchanged: 0,
            credentialBearing: 1,
          },
          assets: [
            {
              assetId: 'a1',
              kind: 'mcp',
              logicalKey: 'm',
              displayName: 'MCP',
              changeKind: 'added',
              hasCredential: true,
              remoteDeleted: false,
            },
          ],
          projectCandidates: [
            {
              hubProjectId: 'hub-x',
              candidateKind: 'hubProjectId',
              candidateExternalId: 'hub-x',
            },
          ],
          resolvedMappings: [],
          plaintextBackupDisclosure: 'plain-git',
          hasCredentialBearingAssets: true,
        }}
        selectedAssetIds={[]}
        mappingDrafts={{ 'hub-x': 'wb-local' }}
        confirmOutcome={null}
        lastMapping={null}
        onInspect={onInspect}
        onSelectLane={onSelectLane}
        onPreview={onPreview}
        onToggleAsset={vi.fn()}
        onMappingDraftChange={onMappingDraftChange}
        onConfirmMapping={onConfirmMapping}
        onConfirmImport={onConfirmImport}
        onClose={vi.fn()}
      />,
    );

    expect(screen.getByTestId('git-import-inspect-step')).toBeTruthy();
    expect(screen.getByTestId('git-import-preview-step')).toBeTruthy();
    expect(screen.getByTestId('git-import-confirm-step')).toBeTruthy();
    expect(screen.getByTestId('git-import-plaintext-disclosure').textContent).toContain('plain-git');

    fireEvent.click(screen.getByTestId('git-import-inspect-btn'));
    expect(onInspect).toHaveBeenCalled();
    // corrupt lane disabled
    expect((screen.getByTestId('git-import-lane-device-bad') as HTMLButtonElement).disabled).toBe(
      true,
    );
    fireEvent.click(screen.getByTestId('git-import-lane-device-b'));
    expect(onSelectLane).toHaveBeenCalledWith('device-b');
    fireEvent.click(screen.getByTestId('git-import-preview-btn'));
    expect(onPreview).toHaveBeenCalled();

    // mapping required before write; confirm mapping explicit
    fireEvent.click(screen.getByTestId('git-import-map-confirm-hub-x'));
    expect(onConfirmMapping).toHaveBeenCalledWith('hub-x');
    expect(screen.getByText(/mapThenOptIn/)).toBeTruthy();

    fireEvent.click(screen.getByTestId('git-import-confirm-btn'));
    expect(onConfirmImport).toHaveBeenCalled();
    // no pull UI
    expect(screen.queryByText(/pull/i)).toBeNull();
  });

  it('treats an explicit empty asset set as no-op and disables confirm', () => {
    render(
      <GitImportDrawer
        open
        busy={false}
        error={null}
        inspectReport={null}
        selectedLaneDeviceId="device-b"
        preview={{
          laneDeviceId: 'device-b',
          snapshotId: 's1',
          snapshotHash: 'hashhashhash',
          sourceReplicaId: 'device-b',
          assetCount: 1,
          revisionCount: 1,
          changeCounts: {
            added: 1,
            modified: 0,
            deleted: 0,
            conflict: 0,
            unchanged: 0,
            credentialBearing: 0,
          },
          assets: [
            {
              assetId: 'a1',
              kind: 'mcp',
              logicalKey: 'm',
              displayName: 'MCP',
              changeKind: 'added',
              hasCredential: false,
              remoteDeleted: false,
            },
          ],
          projectCandidates: [],
          resolvedMappings: [],
          plaintextBackupDisclosure: 'plain-git',
          hasCredentialBearingAssets: false,
        }}
        selectedAssetIds={[]}
        hasExplicitAssetSelection
        mappingDrafts={{}}
        confirmOutcome={null}
        lastMapping={null}
        onInspect={vi.fn()}
        onSelectLane={vi.fn()}
        onPreview={vi.fn()}
        onToggleAsset={vi.fn()}
        onMappingDraftChange={vi.fn()}
        onConfirmMapping={vi.fn()}
        onConfirmImport={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect((screen.getByTestId('git-import-confirm-btn') as HTMLButtonElement).disabled).toBe(
      true,
    );
    expect((screen.getByTestId('git-import-asset-a1') as HTMLInputElement).checked).toBe(false);
  });
});
