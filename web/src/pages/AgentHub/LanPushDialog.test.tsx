/**
 * @vitest-environment jsdom
 */
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { LanPushDialog } from './LanPushDialog';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string) => key,
  }),
}));

describe('LanPushDialog', () => {
  it('selects peers and modes independently and reports each peer', () => {
    const onTogglePeer = vi.fn();
    const onModeChange = vi.fn();
    const onPreview = vi.fn();
    const onStart = vi.fn();
    render(
      <LanPushDialog
        open
        busy={false}
        error={null}
        peers={[
          { deviceId: 'peer-a', name: 'A' },
          { deviceId: 'peer-b', name: 'B' },
        ]}
        selectedPeerIds={['peer-a']}
        onTogglePeer={onTogglePeer}
        mode="fullHub"
        onModeChange={onModeChange}
        assetIdsText=""
        onAssetIdsTextChange={vi.fn()}
        hubProjectIdsText=""
        onHubProjectIdsTextChange={vi.fn()}
        preview={{
          snapshotHash: 'abc',
          snapshotId: 'sid',
          selectionHash: 'sel',
          assetCount: 2,
          revisionCount: 3,
          credentialBearingAssetCount: 1,
          peerDeviceIds: ['peer-a'],
          mode: 'fullHub',
          plaintextBackupDisclosure: 'plain-disclosure',
          hasCredentialBearingAssets: true,
        }}
        report={{
          requestId: 'r1',
          selectionHash: 'sel',
          snapshotHash: 'abc',
          status: 'completed',
          targets: [
            {
              peerDeviceId: 'peer-a',
              peerLabel: 'A',
              clientRequestId: 'r1:peer-a',
              status: 'committed',
              retryable: false,
              missingObjectCount: 0,
              transferredObjectCount: 1,
              updatedAt: '2026-07-29T00:00:00Z',
            },
            {
              peerDeviceId: 'peer-b',
              peerLabel: 'B',
              clientRequestId: 'r1:peer-b',
              status: 'failed',
              retryable: true,
              errorCode: 'timeout',
              missingObjectCount: 0,
              transferredObjectCount: 0,
              updatedAt: '2026-07-29T00:00:00Z',
            },
          ],
        }}
        onPreview={onPreview}
        onStart={onStart}
        onClose={vi.fn()}
      />,
    );

    expect(screen.getByTestId('lan-push-dialog')).toBeTruthy();
    expect(screen.getByTestId('lan-push-plaintext-disclosure').textContent).toContain(
      'plain-disclosure',
    );
    fireEvent.click(screen.getByTestId('lan-push-peer-peer-b'));
    expect(onTogglePeer).toHaveBeenCalledWith('peer-b');
    fireEvent.click(screen.getByTestId('lan-push-mode-project'));
    expect(onModeChange).toHaveBeenCalledWith('project');
    expect(screen.getByTestId('lan-push-target-peer-a').textContent).toContain('committed');
    expect(screen.getByTestId('lan-push-target-peer-b').textContent).toContain('failed');
    // no pull action
    expect(screen.queryByText(/pull/i)).toBeNull();
    fireEvent.click(screen.getByTestId('lan-push-preview-btn'));
    fireEvent.click(screen.getByTestId('lan-push-start-btn'));
    expect(onPreview).toHaveBeenCalled();
    expect(onStart).toHaveBeenCalled();
  });
});
