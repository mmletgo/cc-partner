/**
 * agentAdapterPresentation fail-closed 合同。
 *
 * Business Logic: OpenCode missing bridge 不得 available green；preview deep link 固定 bridge path。
 * Code Logic: pure helper 单测。
 */

import { describe, expect, test } from 'vitest';
import type { OrchestratorAgentAdapterCatalogItem } from '@/lib/types/orchestrator';
import {
  OPENCODE_RUNTIME_BRIDGE_REL_PATH,
  agentAdapterAvailabilityTone,
  agentAdapterBlockedReason,
  agentProviderLabelKey,
  buildOpenCodeBridgeView,
  effectiveOpenCodeBridgeStatus,
  isAgentAdapterEffectivelyAvailable,
  isOpenCodeBridgeReady,
  openCodeBridgePreviewHref,
} from './agentAdapterPresentation';

function openCode(
  overrides: Partial<OrchestratorAgentAdapterCatalogItem> = {},
): OrchestratorAgentAdapterCatalogItem {
  return {
    provider: 'openCodeVisible',
    available: true,
    completionContract: 'hookEvent',
    supportsResume: true,
    supportsUsage: true,
    ...overrides,
  };
}

describe('agentAdapterPresentation', () => {
  test('OpenCode missing bridgeStatus is not effectively available and not success tone', () => {
    const item = openCode({ available: true, bridgeStatus: undefined });
    expect(effectiveOpenCodeBridgeStatus(item)).toBe('previewRequired');
    expect(isAgentAdapterEffectivelyAvailable(item)).toBe(false);
    expect(agentAdapterAvailabilityTone(item)).toBe('warn');
    expect(agentAdapterBlockedReason(item)).toBe('previewRequired');
  });

  test('OpenCode available + ready is effectively available', () => {
    const item = openCode({ available: true, bridgeStatus: 'ready' });
    expect(isOpenCodeBridgeReady(item.bridgeStatus)).toBe(true);
    expect(isAgentAdapterEffectivelyAvailable(item)).toBe(true);
    expect(agentAdapterAvailabilityTone(item)).toBe('success');
  });

  test('OpenCode available true with conflict stays fail-closed', () => {
    const item = openCode({
      available: true,
      bridgeStatus: 'conflict',
      blockedReason: 'external_collision',
    });
    expect(isAgentAdapterEffectivelyAvailable(item)).toBe(false);
    expect(agentAdapterAvailabilityTone(item)).toBe('danger');
    expect(agentAdapterBlockedReason(item)).toBe('external_collision');
  });

  test('preview href targets Agent Hub with fixed bridge path', () => {
    expect(OPENCODE_RUNTIME_BRIDGE_REL_PATH).toBe('.opencode/plugins/cc-partner-runtime.ts');
    expect(openCodeBridgePreviewHref('proj-1')).toBe(
      `/agent-hub?preview=1&bridge=${encodeURIComponent(OPENCODE_RUNTIME_BRIDGE_REL_PATH)}&projectId=proj-1`,
    );
    expect(buildOpenCodeBridgeView(null).requiresProjectPreview).toBe(true);
    expect(buildOpenCodeBridgeView(null).relativePath).toBe(OPENCODE_RUNTIME_BRIDGE_REL_PATH);
  });

  test('provider label keys cover four built-ins', () => {
    expect(agentProviderLabelKey('openCodeVisible')).toBe('providers.openCodeVisible');
    expect(agentProviderLabelKey('claudeCodeVisible')).toBe('providers.claudeCodeVisible');
    expect(agentProviderLabelKey('unknownX')).toBeNull();
  });
});
