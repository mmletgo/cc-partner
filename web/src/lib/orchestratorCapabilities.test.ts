import { describe, expect, test } from 'vitest';
import {
  ORCHESTRATOR_TASK_BLOCKS_CAPABILITY,
  canCreateOrchestratorTaskBlock,
  peerSupportsOrchestratorTaskBlocks,
} from './orchestratorCapabilities';

describe('orchestratorCapabilities', () => {
  test('peerSupportsOrchestratorTaskBlocks matches PeerProtocolInfo.supports', () => {
    expect(peerSupportsOrchestratorTaskBlocks(null)).toBe(false);
    expect(
      peerSupportsOrchestratorTaskBlocks({
        protocol_version: 0,
        capabilities: [ORCHESTRATOR_TASK_BLOCKS_CAPABILITY],
      }),
    ).toBe(false);
    expect(
      peerSupportsOrchestratorTaskBlocks({
        protocol_version: 1,
        capabilities: [],
      }),
    ).toBe(false);
    expect(
      peerSupportsOrchestratorTaskBlocks({
        protocol_version: 1,
        capabilities: [ORCHESTRATOR_TASK_BLOCKS_CAPABILITY],
      }),
    ).toBe(true);
    expect(
      peerSupportsOrchestratorTaskBlocks({
        protoVersion: 1,
        capabilities: [ORCHESTRATOR_TASK_BLOCKS_CAPABILITY],
      }),
    ).toBe(true);
  });

  test('canCreateOrchestratorTaskBlock is local-open and remote fail-closed', () => {
    expect(canCreateOrchestratorTaskBlock({})).toBe(false);
    expect(canCreateOrchestratorTaskBlock({ projectKind: 'local' })).toBe(true);
    expect(canCreateOrchestratorTaskBlock({ projectKind: 'remote' })).toBe(false);
    expect(
      canCreateOrchestratorTaskBlock({
        projectKind: 'remote',
        peer: { protoVersion: 1, capabilities: [ORCHESTRATOR_TASK_BLOCKS_CAPABILITY] },
      }),
    ).toBe(true);
    expect(
      canCreateOrchestratorTaskBlock({
        projectKind: 'remote',
        peer: { protoVersion: 1, capabilities: [] },
      }),
    ).toBe(false);
  });
});
