/**
 * buildExperimentCandidates 只从 effectively available adapter 组 candidate。
 */

import { describe, expect, test } from 'vitest';
import type { OrchestratorAgentAdapterCatalogItem } from '@/lib/types';
import { buildExperimentCandidates } from './useAgentAdapterCatalog';

function adapter(
  provider: string,
  overrides: Partial<OrchestratorAgentAdapterCatalogItem> = {},
): OrchestratorAgentAdapterCatalogItem {
  return {
    provider,
    available: true,
    completionContract: 'hookEvent',
    supportsResume: true,
    supportsUsage: true,
    ...overrides,
  };
}

describe('buildExperimentCandidates', () => {
  test('prefers Claude + ready OpenCode and ignores unavailable Codex', () => {
    expect(
      buildExperimentCandidates([
        adapter('claudeCodeVisible'),
        adapter('codexVisible', { available: false }),
        adapter('openCodeVisible', { bridgeStatus: 'ready' }),
      ]),
    ).toEqual([
      { providerId: 'claudeCodeVisible', strategyLabel: 'baseline' },
      { providerId: 'openCodeVisible', strategyLabel: 'opencode-visible' },
    ]);
  });

  test('falls back to available Codex when OpenCode is not ready', () => {
    expect(
      buildExperimentCandidates([
        adapter('claudeCodeVisible'),
        adapter('codexVisible'),
        adapter('openCodeVisible', { bridgeStatus: 'previewRequired' }),
      ]),
    ).toEqual([
      { providerId: 'claudeCodeVisible', strategyLabel: 'baseline' },
      { providerId: 'codexVisible', strategyLabel: 'codex-visible' },
    ]);
  });

  test('does not invent Claude or Codex when they are unavailable', () => {
    expect(
      buildExperimentCandidates([
        adapter('geminiCliVisible'),
        adapter('cursorCliVisible'),
        adapter('codexVisible', { available: false }),
      ]),
    ).toEqual([
      { providerId: 'geminiCliVisible', strategyLabel: 'geminiCliVisible' },
      { providerId: 'cursorCliVisible', strategyLabel: 'cursorCliVisible' },
    ]);
  });

  test('returns empty when no adapter is effectively available', () => {
    expect(
      buildExperimentCandidates([
        adapter('claudeCodeVisible', { available: false }),
        adapter('openCodeVisible', { available: true, bridgeStatus: undefined }),
      ]),
    ).toEqual([]);
  });

  test('returns a single candidate when only one adapter is available', () => {
    expect(buildExperimentCandidates([adapter('claudeCodeVisible')])).toEqual([
      { providerId: 'claudeCodeVisible', strategyLabel: 'baseline' },
    ]);
  });
});
