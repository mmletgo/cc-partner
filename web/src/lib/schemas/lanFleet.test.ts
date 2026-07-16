/**
 * lanFleet schema 单元测试。
 */
import { describe, expect, it } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import { lanFleetSnapshotDecoder, nonNegativeIntDecoder } from './lanFleet';

/**
 * Business Logic（为什么需要这个函数）:
 *   测试共用最小合法 snapshot。
 *
 * Code Logic（这个函数做什么）:
 *   返回可 decode 的 plain object。
 */
function validSnapshot() {
  return {
    generatedAt: '2026-07-15T00:00:00Z',
    truncated: false,
    devices: [
      {
        deviceId: 'd1',
        deviceName: 'Mac',
        reachability: 'live',
        freshness: 'live',
        schedulerSlotsUsed: 1,
        schedulerSlotsMax: 3,
        projects: [
          {
            projectId: 'p1',
            displayName: 'App',
            projectKind: 'local',
            agentCounts: {
              launching: 0,
              working: 2,
              needsInput: 1,
              idle: 0,
              completed: 0,
              failed: 0,
              disconnected: 0,
            },
            attentionCount: 1,
            terminalCount: 3,
            gitState: 'dirty',
            browserState: 'absent',
            orchestratorRunning: 0,
            orchestratorRetrying: 0,
            lastActivityAt: '2026-07-15T00:01:00Z',
          },
        ],
        errorCode: null,
        capturedAt: '2026-07-15T00:00:00Z',
      },
    ],
  };
}

describe('lanFleet schema', () => {
  it('accepts a valid snapshot', () => {
    const decoded = lanFleetSnapshotDecoder.decode(validSnapshot());
    expect(decoded.devices).toHaveLength(1);
    expect(decoded.devices[0]?.projects[0]?.agentCounts.needsInput).toBe(1);
    expect(decoded.devices[0]?.projects[0]?.gitState).toBe('dirty');
  });

  it('rejects negative counts', () => {
    expect(() => nonNegativeIntDecoder.decode(-1)).toThrow(ContractDecodeError);
    const bad = validSnapshot();
    bad.devices[0]!.projects[0]!.attentionCount = -1;
    expect(() => lanFleetSnapshotDecoder.decode(bad)).toThrow(ContractDecodeError);
  });

  it('rejects invalid freshness but keeps field-level unknown git', () => {
    const badFresh = validSnapshot();
    badFresh.devices[0]!.freshness = 'stale' as 'live';
    expect(() => lanFleetSnapshotDecoder.decode(badFresh)).toThrow(ContractDecodeError);

    const okUnknownGit = validSnapshot();
    okUnknownGit.devices[0]!.projects[0]!.gitState = 'unknown';
    const decoded = lanFleetSnapshotDecoder.decode(okUnknownGit);
    expect(decoded.devices[0]?.projects[0]?.gitState).toBe('unknown');
  });
});
